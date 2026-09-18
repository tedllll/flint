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

/// The same answer, from an endpoint that reports the cache split DeepSeek reports.
fn answers_with_a_cache_split(hit: u64) -> String {
    sse(&[
        r#"data: {"choices":[{"delta":{"content":"hello world"}}]}"#,
        r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
        &format!(
            r#"data: {{"choices":[],"usage":{{"prompt_tokens":1000,"completion_tokens":7,"prompt_cache_hit_tokens":{hit}}}}}"#
        ),
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

/// Run the real binary and hand back what came out of the pipe, unexamined.
///
/// `run_json` above parses the stream, and parsing is what most tests want -- but it is also lossy: a
/// `from_utf8_lossy` turns a bad byte into `U+FFFD` and a `.lines()` hides a carriage return, so a
/// test about the *bytes* has to start from the bytes. This is that entry point.
fn run_bytes(home: &Path, cwd: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_flint"))
        .args(args)
        .arg("--cwd")
        .arg(cwd)
        .env("FLINT_HOME", home)
        .env_remove("NO_COLOR")
        .output()
        .expect("failed to run flint")
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
    // The retry count is always carried, so a caller never has to tell "no retries" from a
    // field that is absent because the endpoint said nothing: flint always knows this one.
    assert_eq!(
        line_of(&lines, "turn.completed")["provider_retries"],
        0,
        "an answer that arrived on the first attempt claims a retry: {}",
        line_of(&lines, "turn.completed")
    );
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

/// The cache split reaches a program reading the stream, and only when the endpoint reported one.
///
/// Both halves matter and the second is the one that is easy to get wrong: a caller that wants to
/// know whether flint's prompt is stable can compute the rate from `cache_hit_tokens`, but a `0`
/// emitted for an endpoint that reports no split would turn "not reported" into "nothing was
/// cached", which is the same defect the terminal line refuses to print.
#[tokio::test]
async fn the_cache_split_reaches_a_program_and_never_as_a_zero() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(SseFixture {
            body: answers_with_a_cache_split(871),
        })
        .mount(&server)
        .await;

    let cwd = cwd_for("cache-json");
    let home = home_for("cache-json", &server.uri(), &cwd);
    let (code, lines, stderr) = run_json(&home, &cwd, &["-p", "say hello", "--json"]);

    assert_eq!(code, 0, "the run failed: {stderr}");
    assert_eq!(line_of(&lines, "usage")["cache_hit_tokens"], 871);
    assert_eq!(line_of(&lines, "turn.completed")["cache_hit_tokens"], 871);

    let plain = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(SseFixture {
            body: sse(&[
                r#"data: {"choices":[{"delta":{"content":"hello world"}}]}"#,
                r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
                r#"data: {"choices":[],"usage":{"prompt_tokens":42,"completion_tokens":11}}"#,
                "data: [DONE]",
            ]),
        })
        .mount(&plain)
        .await;

    let cwd = cwd_for("cache-json-quiet");
    let home = home_for("cache-json-quiet", &plain.uri(), &cwd);
    let (code, lines, stderr) = run_json(&home, &cwd, &["-p", "say hello", "--json"]);

    assert_eq!(code, 0, "the run failed: {stderr}");
    assert_eq!(
        line_of(&lines, "usage")["prompt_tokens"], 42,
        "the plain run is the control: it is the same stream with no cache field in it"
    );
    assert!(
        line_of(&lines, "usage").get("cache_hit_tokens").is_none(),
        "a usage frame with no cache split must not carry the field at all: {}",
        line_of(&lines, "usage")
    );
    assert!(
        line_of(&lines, "turn.completed")
            .get("cache_hit_tokens")
            .is_none(),
        "the same for the end of the turn: {}",
        line_of(&lines, "turn.completed")
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

/// The answer in a file, so a caller that only wants the answer does not have to read a stream.
///
/// Two shapes, and which one a run writes is decided by what was asked for: prose for a run with no
/// schema, the validated object for one with a schema. Both are "the answer" in the sense the
/// caller chose, and the exit code says what it is worth -- `complete`, `incomplete` or `stopped`.
#[tokio::test]
async fn a_result_file_holds_the_answer_the_caller_asked_for() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(SseFixture {
            body: answers_in_two_fragments(),
        })
        .mount(&server)
        .await;

    let cwd = cwd_for("result-file");
    let home = home_for("result-file", &server.uri(), &cwd);
    let answer_file = cwd.join("answer.txt");

    let (code, lines, stderr) = run_json(
        &home,
        &cwd,
        &[
            "-p",
            "say hello",
            "--json",
            "--result-file",
            &answer_file.to_string_lossy(),
        ],
    );

    assert_eq!(code, 0, "the run failed: {stderr} {lines:?}");
    let written = std::fs::read_to_string(&answer_file).expect("the result file");
    assert_eq!(
        written, "hello world",
        "the file does not hold the answer, byte for byte"
    );
    // The same answer the stream carried: two readings of one run, not two answers.
    assert_eq!(line_of(&lines, "message.completed")["text"], written);
}

/// With a schema, the file holds the *validated* value -- the thing the caller asked for -- as JSON.
#[tokio::test]
async fn a_result_file_holds_the_validated_object_when_a_schema_was_given() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(SseFixture {
            body: sse_text(r#"{"trading_day":"2026-09-16"}"#),
        })
        .mount(&server)
        .await;

    let cwd = cwd_for("result-file-schema");
    let home = home_for("result-file-schema", &server.uri(), &cwd);
    let schema_file = cwd.join("schema.json");
    std::fs::write(&schema_file, schema_run()).expect("schema file");
    let answer_file = cwd.join("answer.json");

    let (code, lines, stderr) = run_json(
        &home,
        &cwd,
        &[
            "-p",
            "which trading day",
            "--json",
            "--schema",
            &schema_file.to_string_lossy(),
            "--result-file",
            &answer_file.to_string_lossy(),
        ],
    );

    assert_eq!(code, 0, "the run failed: {stderr} {lines:?}");
    let written = std::fs::read_to_string(&answer_file).expect("the result file");
    let parsed: Value = serde_json::from_str(&written).expect("the result file is not JSON");
    assert_eq!(
        parsed, line_of(&lines, "result")["json"],
        "the file and the stream disagree about the answer"
    );
    assert_eq!(parsed["trading_day"], "2026-09-16");
    // Hand-editable, like everything else flint writes down: not one long line.
    assert!(written.contains('\n'), "the object is on one line: {written:?}");
}

/// A run that produced no answer leaves the file empty, even if it held something before.
///
/// This is the property the flag is worth having: an absent or empty file cannot be mistaken for a
/// value, and a *stale* one is exactly a wrong value that looks right. So the file is claimed when
/// the run starts -- emptied before anything can fail -- and filled only if this run answers.
#[tokio::test]
async fn a_result_file_is_emptied_before_the_run_and_stays_empty_when_nothing_was_answered() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(SseFixture {
            body: answers_in_two_fragments(),
        })
        .mount(&server)
        .await;

    let cwd = cwd_for("result-file-stale");
    let home = home_for("result-file-stale", &server.uri(), &cwd);
    let answer_file = cwd.join("answer.txt");
    std::fs::write(&answer_file, "AN ANSWER FROM AN EARLIER RUN").expect("stale file");

    // A schema this build cannot check: refused while the arguments are resolved, so no turn ever
    // runs and nothing is ever answered.
    let (code, _lines, _stderr) = run_json(
        &home,
        &cwd,
        &[
            "-p",
            "hello",
            "--json",
            "--schema",
            r#"{"type":"object","pattern":1}"#,
            "--result-file",
            &answer_file.to_string_lossy(),
        ],
    );

    assert_ne!(code, 0, "a run that never asked anything must not exit 0");
    assert_eq!(
        std::fs::read_to_string(&answer_file).expect("the result file"),
        "",
        "the earlier run's answer is still there to be read as this run's"
    );
}

/// Without `--json` there is no stream to avoid, so the flag is refused rather than half-supported.
#[tokio::test]
async fn a_result_file_without_json_is_refused() {
    let server = MockServer::start().await;
    let cwd = cwd_for("result-file-plain");
    let home = home_for("result-file-plain", &server.uri(), &cwd);
    let answer_file = cwd.join("answer.txt");

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_flint"))
        .args(["-p", "hello", "--result-file"])
        .arg(&answer_file)
        .arg("--cwd")
        .arg(&cwd)
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .output()
        .expect("failed to run flint");

    assert_eq!(
        out.status.code().unwrap_or(-1),
        exit_codes::USAGE,
        "a flag that cannot be honoured is the caller's command line"
    );
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        stderr.contains("--result-file") && stderr.contains("--json"),
        "the refusal does not say what is missing: {stderr}"
    );
    assert!(
        !answer_file.exists(),
        "a refused run created the file it was told to write"
    );
}

// ---------------------------------------------------------------------------
// `@path`: a document named in the prompt, inlined before the request
// ---------------------------------------------------------------------------

/// A file too big for a command line still reaches the model, because flint inlines it.
///
/// This is the measured wall (`ROADMAP.md` §10, A1): on Windows a 33k prompt fails inside the
/// *caller's* own `subprocess` call with `WinError 206`, before flint has started, so a document
/// cannot travel as an argument at all. This test passes 200 KB through four characters of command
/// line and asserts both halves: the request really carries the contents -- in the prompt, where the
/// model cannot decline to look -- and the stream does not, because the frame is a view and the
/// caller already has the file.
#[tokio::test]
async fn a_file_named_in_the_prompt_reaches_the_model_in_full() {
    let server = MockServer::start().await;
    let cwd = cwd_for("attach-inlined");
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .respond_with(Scripted {
            answers: vec![sse_text("done")],
            seen: std::sync::Arc::clone(&seen),
        })
        .mount(&server)
        .await;

    let body: String = (1..=4000)
        .map(|n| format!("row {n}: 2026-09-{:02}\n", n % 28 + 1))
        .collect();
    std::fs::write(cwd.join("rules.csv"), &body).expect("the fixture file");

    let home = home_for("attach-inlined", &server.uri(), &cwd);
    let (code, lines, stderr) = run_json(
        &home,
        &cwd,
        &["-p", "apply @rules.csv to today", "--json"],
    );
    assert_eq!(code, 0, "the run failed: {stderr} {lines:?}");

    // The turn says what was asked, and what was added to it -- so a caller whose name matched
    // nothing sees an empty list rather than a model quietly answering about a path.
    let started = line_of(&lines, "turn.started");
    assert_eq!(started["prompt"], "apply @rules.csv to today");
    let attached = &started["attachments"][0];
    assert_eq!(attached["token"], "@rules.csv");
    assert_eq!(attached["lines"], 4000);
    assert_eq!(attached["bytes"].as_u64().unwrap(), body.len() as u64);
    assert!(
        std::path::Path::new(attached["path"].as_str().expect("a path")).is_file(),
        "the attachment does not name a file: {attached}"
    );
    assert!(
        !started.to_string().contains("row 4000"),
        "the stream carries the attached document: {}",
        &started.to_string()[..200]
    );

    // ...and the request carries the contents themselves, which is the whole point: content that has
    // to be seen must be *in* the prompt, not a path the model may decline to look up.
    let bodies = recorded(&seen);
    assert_eq!(bodies.len(), 1, "one turn for one answer");
    let sent = bodies[0]["messages"]
        .as_array()
        .expect("messages")
        .last()
        .expect("a user message")["content"]
        .as_str()
        .expect("the user message is text");
    assert!(
        sent.starts_with("apply <file path=\"rules.csv\" lines=\"4000\">\nrow 1:"),
        "the prompt does not open with the file: {}",
        &sent[..120]
    );
    assert!(
        sent.ends_with("</file> to today"),
        "the prose around the name was not kept: {}",
        &sent[sent.len() - 40..]
    );
    assert!(
        sent.contains("row 4000: "),
        "the file was inlined in part, not in full"
    );

    // The session records what the model was given rather than the abbreviation: resuming this
    // conversation has to see the same text, or the record and the request disagree about what was
    // asked.
    let events = session_events(&home);
    let recorded_prompt = events
        .iter()
        .find(|event| event["type"] == "chat" && event["message"]["role"] == "user")
        .and_then(|event| event["message"]["content"].as_str())
        .expect("the user's message in the session");
    assert_eq!(
        recorded_prompt, sent,
        "the session and the request disagree about the prompt"
    );
}

/// A name that is not a file is left exactly as typed, because a prompt is prose.
#[tokio::test]
async fn an_at_that_names_no_file_is_left_in_the_prompt_as_prose() {
    let server = MockServer::start().await;
    let cwd = cwd_for("attach-prose");
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .respond_with(Scripted {
            answers: vec![sse_text("noted")],
            seen: std::sync::Arc::clone(&seen),
        })
        .mount(&server)
        .await;

    let home = home_for("attach-prose", &server.uri(), &cwd);
    let typed = "ask @bob, mail someone@example.com about the @decorator";
    let (code, lines, stderr) = run_json(&home, &cwd, &["-p", typed, "--json"]);
    assert_eq!(code, 0, "the run failed: {stderr} {lines:?}");

    let started = line_of(&lines, "turn.started");
    assert!(
        started.get("attachments").is_none(),
        "a sentence was treated as an attachment: {started}"
    );
    let sent = recorded(&seen)[0]["messages"]
        .as_array()
        .expect("messages")
        .last()
        .expect("a user message")["content"]
        .as_str()
        .expect("the user message is text")
        .to_string();
    assert_eq!(sent, typed, "prose was rewritten");
}

/// A file that cannot be inlined stops the run before anything is sent, and says which one.
#[tokio::test]
async fn a_file_that_cannot_be_inlined_is_refused_before_the_request() {
    let server = MockServer::start().await;
    let cwd = cwd_for("attach-toobig");
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .respond_with(Scripted {
            answers: vec![sse_text("should never be asked")],
            seen: std::sync::Arc::clone(&seen),
        })
        .mount(&server)
        .await;

    std::fs::write(cwd.join("huge.txt"), "x".repeat(300 * 1024)).expect("the fixture file");
    let home = home_for("attach-toobig", &server.uri(), &cwd);
    let (code, lines, stderr) = run_json(&home, &cwd, &["-p", "read @huge.txt", "--json"]);

    assert_eq!(
        code,
        exit_codes::USAGE,
        "an impossible prompt is the caller's command line: {lines:?} {stderr}"
    );
    assert_eq!(
        kinds(&lines),
        vec!["error"],
        "a refused run put something else on the stream: {lines:?}"
    );
    let message = line_of(&lines, "error")["message"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(message.contains("@huge.txt"), "{message}");
    assert!(
        message.contains("262144"),
        "the refusal does not say what the limit is: {message}"
    );
    // Nothing was sent, which is the difference between refusing and paying for an error.
    assert!(
        recorded(&seen).is_empty(),
        "a refused run asked the provider anyway"
    );
}

/// Without a prompt there is no run to describe, and the refusal still arrives as the one thing a
/// `--json` caller reads.
///
/// The rule this pins is the whole of `ROADMAP.md` §10 B7: *every* failure of a `--json` run is on
/// the stream, including the ones decided before the stream is opened. It must not write a *half*
/// stream to get there -- no `session.started`, no `turn.started`, nothing that would let a caller
/// mistake a run that never happened for one that answered nothing -- but silence with a code was
/// the other failure, because it is the one shape a reader of stdout cannot see.
#[tokio::test]
async fn json_without_a_prompt_is_refused_on_the_stream_and_not_silently() {
    let server = MockServer::start().await;
    let cwd = cwd_for("refuse");
    let home = home_for("refuse", &server.uri(), &cwd);
    let (code, lines, stderr) = run_json(&home, &cwd, &["--json"]);

    assert_eq!(code, exit_codes::USAGE, "a missing prompt is the caller's command line");
    assert_eq!(
        kinds(&lines),
        vec!["error"],
        "a refused run has to say so once, on the stream, and nothing else: {lines:?}"
    );
    assert!(
        line_of(&lines, "error")["message"]
            .as_str()
            .unwrap_or_default()
            .contains("--json needs a prompt"),
        "the refusal does not say what is missing: {lines:?}"
    );
    assert_eq!(
        stderr, "",
        "the reason was written to stderr as well; a --json caller reads one channel"
    );
}

/// A command line flint could not finish reading is the same shape, when it had already read `--json`.
///
/// The flag has to have been *read* for flint to know the caller is reading stdout: this is a caller
/// that asked for a stream and then mistyped a flag, and it must not be the one caller whose failure
/// is only on stderr. What is deliberately not claimed is the reverse order -- `flint --nope -p x
/// --json` refuses before it has learned anything, and says so the way every program does.
#[tokio::test]
async fn a_json_run_with_a_mistyped_flag_is_refused_on_the_stream() {
    let server = MockServer::start().await;
    let cwd = cwd_for("refuse-flag");
    let home = home_for("refuse-flag", &server.uri(), &cwd);
    let (code, lines, stderr) = run_json(&home, &cwd, &["-p", "hello", "--json", "--nope"]);

    assert_eq!(code, exit_codes::USAGE, "an unknown flag is the caller's command line");
    assert_eq!(
        kinds(&lines),
        vec!["error"],
        "a refused run has to say so once, on the stream, and nothing else: {lines:?}"
    );
    assert!(
        line_of(&lines, "error")["message"]
            .as_str()
            .unwrap_or_default()
            .contains("--nope"),
        "the refusal does not name the flag: {lines:?}"
    );
    assert_eq!(stderr, "", "the reason was written to stderr as well: {stderr}");
}

/// A caller that has changed its mind can stop the run without killing the process.
///
/// A one-shot run has no keyboard, so `/stop` arrives on stdin -- the same word the REPL takes, for
/// the same reason: it is the interrupt that works when there is no key to press, which is exactly
/// the situation a caller is in. What has to be true: the run ends *promptly* rather than when the
/// model gets around to answering, it ends as a turn rather than as a crash, and the process is still
/// there to be read afterwards -- killing it would take the session file's last writes with it.
/// A turn says how long it took, because "was that slow or was it stuck" is the question a
/// caller asks about a run that sat there for a while, and it is the one thing a caller cannot
/// work out from the outside: timing the subprocess measures flint's start-up and the caller's
/// own reading as well, and a caller that is streaming has no end to time at all.
///
/// The number is checked in both directions. It has to include the model's delay -- a field that
/// reported the milliseconds since the last frame would pass a weaker test -- and it cannot
/// exceed the wall clock the caller just measured for the whole process, which is what makes it
/// this run's clock rather than a constant.
#[tokio::test]
async fn a_turn_says_how_long_it_took() {
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
    Mock::given(method("POST"))
        .respond_with(Slow {
            body: answers_in_two_fragments(),
            delay: std::time::Duration::from_millis(1200),
        })
        .mount(&server)
        .await;

    let cwd = cwd_for("duration");
    let home = home_for("duration", &server.uri(), &cwd);
    let began = std::time::Instant::now();
    let (code, lines, stderr) = run_json(&home, &cwd, &["-p", "say hello", "--json"]);
    let waited = began.elapsed();

    assert_eq!(code, 0, "the run failed: {stderr}");
    let done = line_of(&lines, "turn.completed");
    let ms = done["duration_ms"]
        .as_u64()
        .unwrap_or_else(|| panic!("turn.completed carries no milliseconds: {done}"));
    assert!(
        ms >= 1100,
        "the model was held for 1200 ms and the turn claims {ms} ms"
    );
    assert!(
        ms <= waited.as_millis() as u64,
        "the turn claims {ms} ms inside a {waited:?} run"
    );
}

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
/// A pipe into a run is not a private channel, and the line has to be read anyway.
///
/// `-p --json` reads stdin so a caller can stop a run with `/stop` without killing it, and the price
/// of that is everything else that arrives. It used to be *repeated*: each line came back on stdout as
/// `ignored "…"`, quoted, in full, unbounded -- so a caller that piped a diff, a customer record or a
/// token into a run found it echoed into whatever reads stdout, and a parent agent that shares its
/// stdin with a child found its own protocol lines in flint's output. The count stays, because the
/// other failure is silence: a caller that wrote a line deserves to know it did nothing. What is
/// asserted here is the absence, which is the part that protects a log.
#[tokio::test]
async fn what_arrives_on_stdin_is_counted_and_not_repeated() {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
    let base = format!("http://{}", listener.local_addr().expect("the address"));
    let _talker = std::thread::spawn(move || {
        let Ok((mut socket, _)) = listener.accept() else {
            return;
        };
        let mut request = Vec::new();
        let mut byte = [0u8; 1];
        while !request.windows(4).any(|w| w == b"\r\n\r\n") {
            match socket.read(&mut byte) {
                Ok(0) | Err(_) => return,
                Ok(_) => request.push(byte[0]),
            }
        }
        // The request's body has to be drained before the socket carries the answer. It arrives after
        // the headers, and a socket closed with unread bytes still on it is *reset* -- which the client
        // reports as "error decoding response body" on an answer that arrived complete. The stall
        // fixture below never needed this, because its connection is never closed.
        let headers = String::from_utf8_lossy(&request).to_ascii_lowercase();
        let length: usize = headers
            .lines()
            .find_map(|line| line.strip_prefix("content-length:"))
            .and_then(|value| value.trim().parse().ok())
            .unwrap_or(0);
        if length > 0 {
            let mut body = vec![0u8; length];
            if socket.read_exact(&mut body).is_err() {
                return;
            }
        }
        let _ = socket.write_all(
            b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n",
        );
        let _ = socket.write_all(b"data: {\"choices\":[{\"delta\":{\"content\":\"an answer\"}}]}\n\n");
        let _ = socket.flush();
        // Long enough that the lines below are read while the turn is still going -- which is the
        // only way they can be ignored at all, and the case a caller actually hits.
        std::thread::sleep(std::time::Duration::from_millis(1500));
        let _ = socket.write_all(
            b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
        );
        let _ = socket.flush();
    });

    let cwd = cwd_for("stdin-counted");
    let home = home_for("stdin-counted", &base, &cwd);
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
    // Read as well as the stream, for the failure message: when a run ends early its reason is on
    // stderr, and a test that pipes stderr and never reads it reports only the symptom.
    let mut stderr = child.stderr.take().expect("a pipe");
    let complaints = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });
    {
        let mut stdin = child.stdin.take().expect("a pipe");
        // Shaped like the things that must not come back: a secret, and a line of a parent's own
        // protocol. Both are ordinary text, which is the point -- flint cannot know which is which.
        stdin
            .write_all(b"SECRET-TOKEN-9f3a\nparent-protocol: resume\n")
            .expect("writing to flint");
        stdin.flush().expect("flushing");
    }

    let started = std::time::Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("asking after flint") {
            break status;
        }
        if started.elapsed() > std::time::Duration::from_secs(20) {
            let _ = child.kill();
            break child.wait().expect("reaping flint");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    let text = reader.join().expect("the reader thread");
    let said_when_it_failed = complaints.join().expect("the stderr thread");
    let _ = std::fs::remove_dir_all(&home);

    assert_eq!(
        status.code(),
        Some(0),
        "the run did not finish normally, so the warning below was never reached.\n\
         stream: {text}\nstderr: {said_when_it_failed}"
    );
    assert!(
        !text.contains("SECRET-TOKEN"),
        "what a caller piped in came back out on stdout: {text}"
    );
    assert!(
        !text.contains("parent-protocol"),
        "a line of the caller's own protocol was echoed: {text}"
    );
    // The stream is parsed here rather than searched as text, because "the content is absent" and "the
    // stream is still one object per line" are two promises, and this test is about the first one.
    let lines: Vec<Value> = text
        .lines()
        .map(|line| {
            serde_json::from_str(line).unwrap_or_else(|e| {
                panic!("a line of the stream is not one JSON object: {line:?} ({e})")
            })
        })
        .collect();
    let said = line_of(&lines, "warning")["message"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        said.contains('2') && said.contains("stdin"),
        "a caller that wrote lines is not told they did nothing: {text}"
    );
    assert!(
        said.contains("/stop"),
        "the one line that does something is not named: {text}"
    );
}

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
        // Which limit ran out, as a field rather than as a sentence: raising a step budget and
        // raising a time budget are different repairs, and a caller that has to match on warning
        // text to tell them apart has the fault `outcome` was added to remove.
        assert_eq!(
            line_of(&lines, "turn.completed")["reason"],
            "steps",
            "the turn does not say which limit stopped it: {lines:?}"
        );
    }

    /// A run whose time budget runs out is unfinished, and it is unfinished *now* -- not when the
    /// endpoint finally gets round to answering.
    ///
    /// This is the case a caller cannot otherwise cover: the request is in flight and nothing is
    /// coming back, so the process would sit there for as long as the provider's own timeout allows.
    /// The deadline cuts the turn where it stands, keeps whatever had been drawn, and says which
    /// limit did it.
    #[tokio::test]
    async fn a_run_that_ran_out_of_time_is_unfinished_and_stops_waiting() {
        let server = MockServer::start().await;
        let cwd = cwd_for("code-seconds");
        // Far longer than the budget, and longer than the assertion below is willing to wait: if the
        // deadline did not cut the turn, this test would sit here for half a minute and then fail.
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(sse_text("too late"))
                    .set_delay(std::time::Duration::from_secs(30)),
            )
            .mount(&server)
            .await;
        let home = home_for("code-seconds", &server.uri(), &cwd);

        let began = std::time::Instant::now();
        let (code, lines, stderr) = run_json(
            &home,
            &cwd,
            &["-p", "think about it", "--json", "--max-seconds", "1"],
        );
        let waited = began.elapsed();
        let _ = std::fs::remove_dir_all(&home);

        assert!(
            waited < std::time::Duration::from_secs(15),
            "the run waited {waited:?} for an answer the caller had already given up on"
        );
        assert_eq!(
            line_of(&lines, "turn.completed")["outcome"],
            "incomplete",
            "a run cut short by its budget claimed to have finished: {stderr} {lines:?}"
        );
        assert_eq!(
            line_of(&lines, "turn.completed")["reason"],
            "seconds",
            "the turn does not say which limit stopped it: {lines:?}"
        );
        assert_eq!(
            code, DATAERR,
            "an unfinished answer exited as if it were a usable one: {lines:?}"
        );
        // Said out loud: a caller that reads only the stream has to be able to tell "your budget ran
        // out" from "the model stopped talking".
        let warnings: Vec<String> = lines
            .iter()
            .filter(|line| line["type"] == "warning")
            .map(|line| line["message"].as_str().unwrap_or_default().to_string())
            .collect();
        assert!(
            warnings.iter().any(|w| w.contains("1-second budget")),
            "nothing on the stream says the budget is what ended it: {warnings:?}"
        );
    }

    /// A budget nobody reaches changes nothing: the run ends complete, and says no reason.
    #[tokio::test]
    async fn a_budget_that_is_not_reached_leaves_the_run_alone() {
        let server = MockServer::start().await;
        let cwd = cwd_for("code-seconds-ok");
        Mock::given(method("POST"))
            .respond_with(SseFixture {
                body: answers_in_two_fragments(),
            })
            .mount(&server)
            .await;
        let home = home_for("code-seconds-ok", &server.uri(), &cwd);

        let (code, lines, stderr) = run_json(
            &home,
            &cwd,
            &["-p", "say hello", "--json", "--max-seconds", "30"],
        );
        let _ = std::fs::remove_dir_all(&home);

        assert_eq!(code, 0, "the run failed: {stderr} {lines:?}");
        let ended = line_of(&lines, "turn.completed");
        assert_eq!(ended["outcome"], "complete", "{lines:?}");
        assert!(
            ended.get("reason").is_none(),
            "a finished run carries a reason for stopping: {ended}"
        );
    }

    /// `--max-seconds` is a budget for a call, and a call is a prompt: without one there is nothing
    /// to bound, and a value of zero would mean "no time at all" rather than "no limit".
    #[tokio::test]
    async fn a_budget_without_a_prompt_or_of_zero_is_refused() {
        let server = MockServer::start().await;
        let cwd = cwd_for("code-seconds-usage");
        let home = home_for("code-seconds-usage", &server.uri(), &cwd);

        for args in [
            vec!["--max-seconds", "5", "--json"],
            vec!["-p", "hello", "--json", "--max-seconds", "0"],
            vec!["-p", "hello", "--json", "--max-seconds", "soon"],
        ] {
            let (code, lines, _stderr) = run_json(&home, &cwd, &args);
            assert_eq!(
                code, USAGE,
                "{args:?} was not refused as a command line: {lines:?}"
            );
            assert_eq!(
                kinds(&lines),
                vec!["error"],
                "{args:?} put half a run on the stream: {lines:?}"
            );
        }
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

    /// A turn that needed two attempts says so, because that is the one part of "why was that slow"
    /// that `duration_ms` can raise without being able to answer it.
    ///
    /// The retry itself is invisible to the model and to the answer: the first attempt failed before
    /// anything had been drawn, so nothing of it reached the stream, and without a count the only
    /// trace was a notice on stderr -- which a `--json` caller does not read, and which a log read
    /// afterwards cannot count. The provider's own ladder is what is measured here: one 503, then the
    /// answer, and the turn reports one retry rather than looking like a slow first attempt.
    #[tokio::test]
    async fn a_retried_attempt_is_counted_on_the_turn() {
        let server = MockServer::start().await;
        let cwd = cwd_for("code-retries");
        std::fs::create_dir_all(&cwd).expect("working directory");
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(503).set_body_string("upstream is busy"))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(SseFixture {
                body: answers_in_two_fragments(),
            })
            .with_priority(2)
            .mount(&server)
            .await;

        let home = home_for("code-retries", &server.uri(), &cwd);
        let (code, lines, stderr) = run_json(&home, &cwd, &["-p", "say hello", "--json"]);
        let _ = std::fs::remove_dir_all(&home);

        assert_eq!(code, 0, "the second attempt should have answered: {stderr}");
        assert_eq!(line_of(&lines, "message.completed")["text"], "hello world");
        assert_eq!(
            line_of(&lines, "turn.completed")["provider_retries"],
            1,
            "the turn that needed two attempts reports one retry: {}",
            line_of(&lines, "turn.completed")
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
        // A home with a provider in it, so the only thing that can fail is the schema: an empty home
        // makes `Config::load` print its first-run banner to stderr, which is a different question
        // (that banner is for a person, and it is written before any of this is resolved).
        let home = home_configured(
            "code-schema",
            "",
            "name = \"stub\"\nbase_url = \"http://127.0.0.1:9/v1\"\napi_key = \"test\"\nmodel = \"stub-model\"",
        );

        let (code, lines, stderr) = run_json(
            &home,
            &cwd,
            &["-p", "hello", "--json", "--schema", r#"{"type":"object","pattern":1}"#],
        );
        let _ = std::fs::remove_dir_all(&home);
        // The refusal is resolved before the stream is opened, and it still arrives on the stream:
        // this is the case `ROADMAP.md` §10 B7 was written from, and the code is the other half of
        // it. One `error` line and nothing else -- no `session.started`, because nothing started.
        assert_eq!(
            kinds(&lines),
            vec!["error"],
            "a run refused before it started did not describe the refusal on its own stream: {lines:?}"
        );
        let message = line_of(&lines, "error")["message"]
            .as_str()
            .unwrap_or_default();
        assert!(
            message.contains("pattern") || message.contains("schema"),
            "the refusal does not say what is wrong: {message}"
        );
        assert_eq!(
            stderr, "",
            "the reason was written to stderr as well; a --json caller reads one channel"
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
///
/// And with a **combination a reader is likely to want to "fix"**: `turn.completed` says
/// `outcome: "complete"`, and *then* an `error` arrives and the process exits 65. That is deliberate
/// and the assertion below is the reason it is written down -- the turn really did finish (three
/// answers were produced, and flint stopped asking because the schema kept refusing them), while the
/// answer is unusable. Two signals answer two questions: `outcome` is about the turn, the `error` and
/// the exit code are about the answer. Making either one agree with the other would throw away one of
/// those facts: `incomplete` would blame a step limit that never ran out, and a `turn.completed` after
/// the `error` would read as a turn that finished after it had been reported as failed.
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
    // The combination, in order: the turn's own end comes first and calls itself complete, and the
    // unusable answer is reported after it. The order is load-bearing -- a caller that stops reading
    // at `turn.completed` (the terminal frame, which is where a streaming caller is told it can stop
    // waiting) still gets the `error` only by reading one more line, which is why the frame's own
    // comment says the two answer different questions rather than that one of them is wrong.
    let order = kinds(&lines);
    let turned = order
        .iter()
        .position(|k| *k == "turn.completed")
        .unwrap_or_else(|| panic!("no turn.completed in {:?}", kinds(&lines)));
    let errored = order
        .iter()
        .position(|k| *k == "error")
        .unwrap_or_else(|| panic!("no error in {:?}", kinds(&lines)));
    assert!(
        turned < errored,
        "the answer's failure is reported before the turn's end: {:?}",
        kinds(&lines)
    );
    assert_eq!(
        line_of(&lines, "turn.completed")["outcome"],
        "complete",
        "the turn is being reported as something other than what it was: {}",
        line_of(&lines, "turn.completed")
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
    // On the stream, once, and not on stderr: the refusal happens before the stream is opened and
    // still belongs to the caller that asked for one (§10 B7).
    assert_eq!(
        kinds(&lines),
        vec!["error"],
        "a refused run still wrote a stream: {lines:?}"
    );
    assert!(
        line_of(&lines, "error")["message"]
            .as_str()
            .unwrap_or_default()
            .contains("oneOf"),
        "the refusal does not name the keyword flint cannot check: {lines:?}"
    );
    assert_eq!(stderr, "", "the refusal went to stderr as well: {stderr}");
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

/// Every frame type `--json` may write, as one list.
///
/// The vocabulary is closed, which until now was a sentence in the README and a convention in
/// `src/ndjson.rs`. This is the copy that fails when it grows: a new frame type has to be added here
/// by hand, and that is the moment somebody reads the README and notices it is out of date too -- which
/// it was, by two types (`status`, the heartbeat a silent turn carries, and `command`, the output of a
/// slash command in the page's feed).
const VOCABULARY: &[&str] = &[
    "session.started",
    "turn.started",
    "message.delta",
    "reasoning.delta",
    "message.completed",
    "tool.started",
    "tool.args",
    "tool.completed",
    "usage",
    "status",
    "command",
    "warning",
    "error",
    "result",
    "turn.completed",
];

/// Check one run's bytes against the promise `--json` makes about them.
///
/// Everything here is about the pipe, not about the run: what the frames *say* is the other tests'
/// business. The reasons each line matters are worth keeping together, because a caller's parser is
/// built on all of them at once -- it splits on `\n`, so a line that is not an object is a crash; it
/// decodes as UTF-8, so one bad byte is a crash; and it may be reading a terminal's leavings, so an
/// escape code or a `\r` is a frame that never parses.
fn assert_only_frames(tag: &str, out: &std::process::Output) {
    let stdout = std::str::from_utf8(&out.stdout)
        .unwrap_or_else(|e| panic!("{tag}: stdout is not UTF-8 at byte {} ({e})", e.valid_up_to()));

    assert!(!stdout.is_empty(), "{tag}: a --json run wrote nothing at all");
    assert!(
        stdout.ends_with('\n'),
        "{tag}: the last frame has no newline, so a reader that waits for a line waits for ever"
    );
    assert!(
        !stdout.contains('\r'),
        "{tag}: a carriage return reached stdout -- a line ending from somewhere that is not the stream"
    );
    assert!(
        !stdout.contains('\u{1b}'),
        "{tag}: an escape code reached stdout, so a caller that is not a terminal is reading paint"
    );

    let mut seen: Vec<String> = Vec::new();
    for (number, line) in stdout.lines().enumerate() {
        let line = line.trim_end_matches('\n');
        assert!(
            !line.trim().is_empty(),
            "{tag}: line {} is blank; a blank line is not a frame",
            number + 1
        );
        assert_eq!(
            line,
            line.trim(),
            "{tag}: line {} has whitespace around it, which is not how a frame is written",
            number + 1
        );
        let value: Value = serde_json::from_str(line).unwrap_or_else(|e| {
            panic!("{tag}: line {} is not JSON: {line:?} ({e})", number + 1)
        });
        let object = value.as_object().unwrap_or_else(|| {
            panic!("{tag}: line {} is JSON but not an object: {line:?}", number + 1)
        });
        let kind = object
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or_else(|| panic!("{tag}: line {} has no type: {line:?}", number + 1));
        assert!(
            VOCABULARY.contains(&kind),
            "{tag}: line {} is a frame type nobody documented: {kind:?} -- if it is new, add it to \
             VOCABULARY here and to the list in README.md",
            number + 1
        );
        seen.push(kind.to_string());
    }
    assert!(
        !seen.is_empty(),
        "{tag}: every line was filtered out, so this checked nothing"
    );
}

/// The stream is frames and nothing else, byte for byte, in every shape a run can take.
///
/// The other tests in this file read the stream through `run_json`, which is lossy in the two ways that
/// matter (a `from_utf8_lossy` and a `.lines()`): a stray raw byte, a `\r`, or an escape code from the
/// terminal path would survive all of them. And a stray `println!` reaches whichever shape its author
/// was working on, so one happy path is not evidence -- the four below are an answer, a tool round, a
/// refusal that never reached the model, and a run cut short by its own budget.
#[tokio::test]
async fn the_stream_is_frames_and_nothing_else_on_the_bytes() {
    let server = MockServer::start().await;
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let cwd = cwd_for("bytes");
    // One stub, two answers: a tool call and then the text after it, which is what puts every
    // `tool.*` frame and the round trip after it into the stream.
    let note = cwd.join("note.txt");
    std::fs::write(&note, "the note\n").expect("the fixture file");
    Mock::given(method("POST"))
        .respond_with(Scripted {
            answers: vec![
                asks_to_read("call_1", note.to_string_lossy().as_ref()),
                sse_text("read it"),
            ],
            seen: std::sync::Arc::clone(&seen),
        })
        .mount(&server)
        .await;
    let home = home_for("bytes", &server.uri(), &cwd);

    // 1. A tool round and the answer after it: the busiest the stream ever gets.
    let out = run_bytes(&home, &cwd, &["-p", "read note.txt and tell me", "--json"]);
    assert_eq!(out.status.code(), Some(0), "the run failed: {:?}", out);
    assert_only_frames("tool round", &out);
    assert_eq!(
        out.stderr,
        Vec::<u8>::new(),
        "a --json run wrote to stderr, which is not the channel it promised"
    );
    // The tool round has to have happened, or this checked the easy shape four times.
    assert!(
        !recorded(&seen).is_empty() && recorded(&seen).len() > 1,
        "the stub was asked once, so no tool round is in that stream"
    );

    // 2. A refusal while the command line is being read: no model, no session, one frame.
    let out = run_bytes(&home, &cwd, &["-p", "hello", "--json", "--nope"]);
    assert_eq!(out.status.code(), Some(2), "a bad flag is the caller's input");
    assert_only_frames("refusal", &out);

    // 3. An answer, the ordinary shape: two deltas, the whole text, and the totals.
    let server2 = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(SseFixture {
            body: answers_in_two_fragments(),
        })
        .mount(&server2)
        .await;
    let home2 = home_for("bytes-answer", &server2.uri(), &cwd);
    let out = run_bytes(&home2, &cwd, &["-p", "say hello", "--json"]);
    assert_eq!(out.status.code(), Some(0), "the run failed: {:?}", out);
    assert_only_frames("answer", &out);

    // 4. A run whose budget ran out: a warning, an unfinished turn, and a non-zero code. The half
    // answer is the interesting one -- text that arrived *after* the deadline must not reach the
    // stream as though it were in time.
    let server3 = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(answers_in_two_fragments())
                .set_delay(std::time::Duration::from_secs(30)),
        )
        .mount(&server3)
        .await;
    let home3 = home_for("bytes-budget", &server3.uri(), &cwd);
    let out = run_bytes(
        &home3,
        &cwd,
        &["-p", "say hello", "--json", "--max-seconds", "1"],
    );
    assert_eq!(out.status.code(), Some(65), "the budget did not cut the run");
    assert_only_frames("budget", &out);

    for dir in [home, home2, home3] {
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A reasoning level rides in the field the provider names, and in no other.
///
/// The value is flint's word and the field is the endpoint's, which is the whole design: the level
/// ladder is one small vocabulary a person learns once, and the JSON key it goes in is named in the
/// provider's own config. This watches both halves on the wire -- the field the endpoint was told to
/// expect, and the absence of the key flint would have guessed at otherwise.
#[tokio::test]
async fn a_thinking_level_rides_in_the_field_the_provider_names() {
    let server = MockServer::start().await;
    let cwd = cwd_for("thinking-level");
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .respond_with(Scripted {
            answers: vec![sse_text("reasoning hard.")],
            seen: std::sync::Arc::clone(&seen),
        })
        .mount(&server)
        .await;

    let home = home_configured(
        "thinking-level",
        "",
        &format!(
            "name = \"stub\"\nbase_url = \"{}\"\napi_key = \"test\"\nmodel = \"stub-model\"\n\
             thinking_field = \"reasoning_effort\"\n",
            server.uri()
        ),
    );
    let (code, lines, stderr) = run_json(
        &home,
        &cwd,
        &["-p", "think about it", "--json", "--thinking", "high"],
    );
    assert_eq!(code, 0, "the run failed: {stderr} {lines:?}");

    let bodies = recorded(&seen);
    assert_eq!(bodies.len(), 1, "one turn for one answer");
    assert_eq!(
        bodies[0]["reasoning_effort"], "high",
        "the level did not reach the request: {}",
        bodies[0]
    );
}

/// Nothing is asked for unless a level was asked for.
///
/// The default is the request body flint has always sent, and it has to stay that way: a provider
/// whose `thinking_field` is written down is a provider that *can* be asked, not one that is. A
/// flint that sent `reasoning_effort: "off"` on its own would be inventing a word for a vendor it
/// knows nothing about, and the endpoints that refuse an unknown effort value refuse the turn.
#[tokio::test]
async fn no_reasoning_is_asked_for_until_somebody_asks() {
    let server = MockServer::start().await;
    let cwd = cwd_for("thinking-default");
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .respond_with(Scripted {
            answers: vec![sse_text("plain.")],
            seen: std::sync::Arc::clone(&seen),
        })
        .mount(&server)
        .await;

    let home = home_configured(
        "thinking-default",
        "",
        &format!(
            "name = \"stub\"\nbase_url = \"{}\"\napi_key = \"test\"\nmodel = \"stub-model\"\n\
             thinking_field = \"reasoning_effort\"\n",
            server.uri()
        ),
    );
    let (code, _lines, stderr) = run_json(&home, &cwd, &["-p", "say hello", "--json"]);
    assert_eq!(code, 0, "the run failed: {stderr}");

    let bodies = recorded(&seen);
    assert!(
        bodies[0].get("reasoning_effort").is_none(),
        "a level nobody asked for was sent: {}",
        bodies[0]
    );
}
/// A level is written into the conversation's own file, and comes back with it.
///
/// Both halves matter and neither is visible from one run. Writing it down is what makes the choice
/// survive the run that made it; reading it back is what makes a resumed conversation ask for the
/// same reasoning *without* being told again -- and the silent failure of the second half is a
/// conversation that looks resumed and has quietly gone back to the endpoint's default.
#[tokio::test]
async fn a_reasoning_level_comes_back_with_the_conversation() {
    let server = MockServer::start().await;
    let cwd = cwd_for("thinking-travels");
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .respond_with(Scripted {
            answers: vec![sse_text("first."), sse_text("second.")],
            seen: std::sync::Arc::clone(&seen),
        })
        .mount(&server)
        .await;

    let home = home_configured(
        "thinking-travels",
        "",
        &format!(
            "name = \"stub\"\nbase_url = \"{}\"\napi_key = \"test\"\nmodel = \"stub-model\"\n\
             thinking_field = \"reasoning_effort\"\n",
            server.uri()
        ),
    );
    let (code, _lines, stderr) = run_json(
        &home,
        &cwd,
        &["-p", "first", "--json", "--thinking", "high"],
    );
    assert_eq!(code, 0, "the first run failed: {stderr}");

    let events = session_events(&home);
    let written = events
        .iter()
        .find(|event| event["type"] == "thinking")
        .unwrap_or_else(|| panic!("no thinking line in the session: {events:?}"));
    assert_eq!(written["level"], "high");

    // Resumed with no flag at all: the file has the last word over the config's `off`.
    let session = session_files(&home);
    let path = session[0].to_str().expect("a session path");
    let (code, _lines, stderr) = run_json(&home, &cwd, &["--resume", path, "-p", "second", "--json"]);
    assert_eq!(code, 0, "the resumed run failed: {stderr}");

    let bodies = recorded(&seen);
    assert_eq!(
        bodies.last().expect("a second request")["reasoning_effort"],
        "high",
        "the resumed run did not ask for the level its own file records"
    );
}