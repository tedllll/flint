//! The `task` tool: one flint starting another.
//!
//! The child is a real process -- the same binary this test is built beside -- so this exercises the
//! door end to end: the argv, the child's `--json` stream, its exit code, the session path it hands
//! back, and the one block of text the model receives. A scripted stub provider answers in the order
//! requests arrive, which is what makes "the parent asked, the child worked, the parent finished"
//! observable without a key or a network.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

fn sse(lines: &[&str]) -> String {
    let mut out = String::new();
    for line in lines {
        out.push_str(line);
        out.push_str("\n\n");
    }
    out
}

/// One SSE body a model would send: either prose or a tool call.
fn prose(text: &str) -> String {
    sse(&[
        &format!(r#"data: {{"choices":[{{"delta":{{"content":"{text}"}}}}]}}"#),
        r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
        r#"data: {"choices":[],"usage":{"prompt_tokens":7,"completion_tokens":3}}"#,
        "data: [DONE]",
    ])
}

fn tool_call(name: &str, args: &str) -> String {
    sse(&[
        &format!(
            r#"data: {{"choices":[{{"delta":{{"tool_calls":[{{"index":0,"id":"call_1","function":{{"name":"{name}","arguments":{}}}}}]}}}}]}}"#,
            serde_json::to_string(args).expect("arguments are a string")
        ),
        r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        "data: [DONE]",
    ])
}

/// Answers by request number, so that the parent's turn and the child's turn can differ.
struct Scripted {
    step: Arc<AtomicUsize>,
    /// Indexed by request number; the last one repeats.
    bodies: Vec<String>,
}

impl Respond for Scripted {
    fn respond(&self, _req: &Request) -> ResponseTemplate {
        let n = self.step.fetch_add(1, Ordering::SeqCst);
        let body = self
            .bodies
            .get(n)
            .or_else(|| self.bodies.last())
            .cloned()
            .unwrap_or_else(|| prose("NOTHING SCRIPTED"));
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string(body)
    }
}

/// A `FLINT_HOME` of its own with the stub as its default provider, and a directory to work in.
///
/// The child inherits the environment, so it reads the same config and talks to the same stub --
/// which is the point: "another flint like this one" has to mean this one.
fn scratch(tag: &str, base_url: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("flint-task-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    std::fs::write(
        dir.join("config.toml"),
        format!(
            "default_provider = \"stub\"\n\n\
             [[providers]]\n\
             name = \"stub\"\n\
             base_url = \"{base_url}\"\n\
             model = \"stub-model\"\n\
             api_key = \"not-a-real-key\"\n"
        ),
    )
    .expect("test config");
    dir
}

fn binary() -> std::process::Command {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_flint"));
    // The child is started from this binary, so a `FLINT_BIN` left over in the environment would
    // send it somewhere else entirely.
    cmd.env_remove("FLINT_BIN");
    cmd
}

/// Run one flint, with `FLINT_HOME` in `home` and the working directory in `work`, and give back
/// (exit code, stdout, stderr).
fn run_flint(home: &Path, work: &Path, extra: &[&str]) -> (i32, String, String) {
    let mut args: Vec<String> = vec![
        "-p".to_string(),
        "ask the child".to_string(),
        "--json".to_string(),
        "--cwd".to_string(),
        work.display().to_string(),
    ];
    args.extend(extra.iter().map(|s| s.to_string()));
    let out = binary()
        .args(&args)
        .env("FLINT_HOME", home)
        .env_remove("FLINT_DEPTH")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("failed to run flint");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// The session file this run wrote, taken from its own `--json` stream.
fn session_of(stdout: &str) -> PathBuf {
    for line in stdout.lines() {
        let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if event.get("type").and_then(|v| v.as_str()) == Some("session.started") {
            if let Some(path) = event.get("session").and_then(|v| v.as_str()) {
                return PathBuf::from(path);
            }
        }
    }
    panic!("no session.started frame in: {stdout}");
}

/// Everything the parent recorded, tool results included. The transcript is the evidence here: the
/// model's own view of the child is the tool result, and that is what has to be checked.
///
/// Session lines are JSON, so a tool result sits escaped inside a string. Every string in every line
/// is collected and joined; that is what makes an assertion about a *line* of the result --
/// `session: …` -- mean what a reader of the transcript would mean by it.
fn transcript(session: &Path) -> String {
    let raw =
        std::fs::read_to_string(session).unwrap_or_else(|e| panic!("reading {session:?}: {e}"));
    let mut out = String::new();
    for line in raw.lines() {
        match serde_json::from_str::<serde_json::Value>(line) {
            Ok(event) => collect_strings(&event, &mut out),
            Err(_) => {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    out
}

fn collect_strings(value: &serde_json::Value, out: &mut String) {
    match value {
        serde_json::Value::String(text) => {
            out.push_str(text);
            out.push('\n');
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_strings(item, out);
            }
        }
        serde_json::Value::Object(fields) => {
            for (_, field) in fields {
                collect_strings(field, out);
            }
        }
        _ => {}
    }
}

/// The session paths named inside a tool result, so a test can follow the provenance chain.
fn sessions_named(text: &str) -> Vec<PathBuf> {
    text.lines()
        .filter_map(|line| line.strip_prefix("session: "))
        .map(|p| PathBuf::from(p.trim()))
        .collect()
}

#[tokio::test]
async fn a_child_run_is_a_real_run_and_its_answer_comes_back_with_its_provenance() {
    let step = Arc::new(AtomicUsize::new(0));
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(Scripted {
            step: step.clone(),
            bodies: vec![
                // The parent asks for a child.
                tool_call("task", r#"{"prompt":"what is in the box?"}"#),
                // The child answers -- and it is the child's second request overall, because the
                // stub cannot tell the two runs apart except by order.
                prose("CHILD FOUND THE ANSWER"),
                // The parent, having read the tool result, finishes.
                prose("PARENT DONE"),
            ],
        })
        .mount(&server)
        .await;

    let home = scratch("basic", &server.uri());
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("work dir");

    let (code, stdout, stderr) = run_flint(&home, &work, &[]);
    assert_eq!(code, 0, "flint failed: {stderr}");
    assert!(
        stdout.contains("PARENT DONE"),
        "the parent did not finish: {stdout}"
    );

    let session = session_of(&stdout);
    let text = transcript(&session);
    assert!(
        text.contains("CHILD FOUND THE ANSWER"),
        "the child's answer never reached the parent's transcript"
    );
    // The facts a caller needs to judge the answer, attached to it rather than left to be guessed.
    assert!(text.contains("exit code: 0 (finished)"), "{text}");
    assert!(text.contains("outcome: complete"), "{text}");
    assert!(text.contains("readonly: false"), "{text}");
    assert!(text.contains("depth: 1 (this run is 0)"), "{text}");

    // The provenance chain: the result names the child's session, and that file really exists and
    // really holds the conversation. This is what makes a value traceable to the run that produced
    // it, which is the difference between a subagent and a shell pipeline.
    let named = sessions_named(&text);
    assert_eq!(named.len(), 1, "expected one child session, got {named:?}");
    let child_session = &named[0];
    assert!(
        child_session.exists(),
        "the child session named in the result does not exist: {child_session:?}"
    );
    let child_text = transcript(child_session);
    assert!(
        child_text.contains("what is in the box?"),
        "the child's session does not hold the prompt it was asked"
    );
    assert!(
        child_text.contains("CHILD FOUND THE ANSWER"),
        "the child's session does not hold its answer"
    );

    // Three requests: parent asks, child answers, parent finishes. Nothing looped.
    assert_eq!(step.load(Ordering::SeqCst), 3);

    let _ = std::fs::remove_dir_all(&home);
}

#[tokio::test]
async fn a_readonly_run_cannot_be_talked_into_a_writing_child() {
    let step = Arc::new(AtomicUsize::new(0));
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(Scripted {
            step: step.clone(),
            bodies: vec![
                // The model asks for a writing child while *its own* run is readonly -- the exact
                // attempt the monotonic rule exists to refuse.
                tool_call("task", r#"{"prompt":"write the file","readonly":false}"#),
                // The child tries to write. In the readonly run it must be refused there too.
                tool_call(
                    "write",
                    r#"{"path":"child.txt","content":"written by a child"}"#,
                ),
                prose("CHILD FINISHED"),
                prose("PARENT DONE"),
            ],
        })
        .mount(&server)
        .await;

    let home = scratch("readonly", &server.uri());
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("work dir");

    // First: as a readonly run.
    let (code, stdout, stderr) = run_flint(&home, &work, &["--readonly"]);
    assert_eq!(code, 0, "flint failed: {stderr}");
    let text = transcript(&session_of(&stdout));
    assert!(
        text.contains("readonly: true"),
        "the child was not readonly despite the parent being readonly: {text}"
    );
    assert!(
        !work.join("child.txt").exists(),
        "a readonly run produced a writing child, so `readonly` means nothing"
    );

    // Then the same script with a writable parent, so the test shows the refusal is the flag and not
    // the child being unable to write at all.
    step.store(0, Ordering::SeqCst);
    let (code, stdout, stderr) = run_flint(&home, &work, &[]);
    assert_eq!(code, 0, "flint failed: {stderr}");
    let text = transcript(&session_of(&stdout));
    assert!(
        text.contains("readonly: false"),
        "the child should have been writable here: {text}"
    );
    assert!(
        work.join("child.txt").exists(),
        "the child did not write, so the readonly half of this test proves nothing"
    );

    let _ = std::fs::remove_dir_all(&home);
}

#[tokio::test]
async fn a_run_that_is_already_deep_refuses_to_go_deeper() {
    let step = Arc::new(AtomicUsize::new(0));
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(Scripted {
            step: step.clone(),
            bodies: vec![
                tool_call("task", r#"{"prompt":"go deeper"}"#),
                prose("PARENT DONE"),
            ],
        })
        .mount(&server)
        .await;

    let home = scratch("depth", &server.uri());
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("work dir");

    // The run is told it is already at the limit, which is how the tool writes it for a child and
    // the only way anything sets it: there is no flag, so a model cannot edit the bound out of its
    // own command line.
    let out = binary()
        .args([
            "-p",
            "ask the child",
            "--json",
            "--cwd",
            &work.display().to_string(),
        ])
        .env("FLINT_HOME", &home)
        .env("FLINT_DEPTH", "2")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("failed to run flint");
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let text = transcript(&session_of(&stdout));

    assert!(
        text.contains("does not go deeper than 2"),
        "the depth limit was not reported: {text}"
    );
    assert!(
        text.contains("at depth 2"),
        "the refusal did not say where the run was: {text}"
    );
    // Refused, not attempted: only the parent's two turns happened.
    assert_eq!(
        step.load(Ordering::SeqCst),
        2,
        "a child was started despite the depth limit"
    );

    let _ = std::fs::remove_dir_all(&home);
}
