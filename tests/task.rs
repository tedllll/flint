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

/// Answers by request number like `Scripted`, and remembers what was asked and when.
///
/// The time matters for the fan-out: whether three children ran at the same time is a fact about
/// when their requests *arrived*, and nothing else in this test can see it. `delay` holds each answer
/// so that "at the same time" and "one after another" are distinguishable by the clock rather than by
/// the order of a list.
struct Watched {
    step: Arc<AtomicUsize>,
    bodies: Vec<String>,
    seen: Arc<std::sync::Mutex<Vec<(String, std::time::Instant)>>>,
    delay: std::time::Duration,
}

impl Respond for Watched {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let body = String::from_utf8_lossy(&req.body).to_string();
        self.seen
            .lock()
            .expect("seen lock")
            .push((body, std::time::Instant::now()));
        let n = self.step.fetch_add(1, Ordering::SeqCst);
        let body = self
            .bodies
            .get(n)
            .or_else(|| self.bodies.last())
            .cloned()
            .unwrap_or_else(|| prose("NOTHING SCRIPTED"));
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_delay(self.delay)
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

/// Every conversation file under a home, at any depth.
///
/// The mailbox is not one: it is the same `.jsonl` idea under `mailbox/`, and it is not a
/// conversation. So the walk starts at `sessions/`, which is the directory every door a run can open
/// writes into -- including `children/`, one level down, which is the level a listing does not read
/// and therefore the one a test about "no files at all" has to look at.
fn session_files(home: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(&home.join("sessions"), &mut out);
    out.sort();
    out
}

/// A run that keeps no conversation starts children that keep none either.
///
/// The child is the one door such a run opens by itself, and the file it would leave is worse than
/// untidy: a parent with no session has no id to hand down as `FLINT_PARENT`, so the child's
/// conversation is not filed under `children/` -- it lands in the person's own list, a conversation
/// they never had, written by a run that promised to write none. The child really runs here (its
/// answer is in the parent's transcript), because a home with no files proves nothing if the child
/// never started.
#[tokio::test]
async fn a_run_that_keeps_no_conversation_starts_children_that_keep_none() {
    let step = Arc::new(AtomicUsize::new(0));
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(Scripted {
            step: step.clone(),
            bodies: vec![
                // The parent waits: this test is about what the child leaves behind, and it has to
                // have run to completion for "nothing was written" to be an answer rather than a
                // race. `background: false` is explicit because the default is the handle.
                tool_call(
                    "task",
                    r#"{"prompt":"what is in the box?","background":false}"#,
                ),
                prose("CHILD FOUND THE ANSWER"),
                prose("PARENT DONE"),
            ],
        })
        .mount(&server)
        .await;

    let home = scratch("keeps-nothing", &server.uri());
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("work dir");
    let (code, stdout, stderr) = run_flint(&home, &work, &["--no-session"]);
    assert_eq!(code, 0, "flint failed: {stderr}");
    assert!(
        stdout.contains("CHILD FOUND THE ANSWER"),
        "the child never ran, so an empty home would prove nothing: {stdout}"
    );

    // The parent says so itself, rather than leaving the `session` field out for a caller to read as
    // "not yet".
    let started = stdout
        .lines()
        .find(|line| line.contains(r#""type":"session.started""#))
        .unwrap_or_else(|| panic!("no session.started frame: {stdout}"));
    assert!(
        started.contains(r#""session":null"#),
        "the parent named a conversation it was told not to keep: {started}"
    );
    // And the child's answer comes back saying the same thing, because the result is where a caller
    // looks to carry on inside the child's conversation.
    assert!(
        stdout.contains("session: none (--no-session)"),
        "the child's result is silent about the conversation it does not have: {stdout}"
    );

    let files = session_files(&home);
    assert!(
        files.is_empty(),
        "a conversation was written by a run that keeps none: {files:?}"
    );
    let _ = std::fs::remove_dir_all(&home);
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
                // The parent asks for a child *and waits for it*: this test is about the answer
                // arriving with its provenance, so the door that hands the answer over is the one it
                // takes. `background: false` is explicit because the default is now the handle.
                tool_call("task", r#"{"prompt":"what is in the box?","background":false}"#),
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

/// A child's conversation is not one of the person's, wherever the person looks for theirs.
///
/// Reported from a real session, and the sharp edge is `--continue`: a child is *newer* than the parent
/// that started it, so "the newest conversation in this directory" answered with the child's. The same
/// listing stands behind `/sessions`, `flint --list-sessions` and the page's sidebar, and behind the
/// numbers `--resume N` takes, so there is one rule for all of them: a child's file lives under
/// `children/`, which none of them read. The conversation itself is unchanged -- same format, same
/// readability -- which is what the first test in this file asserts.
#[tokio::test]
async fn a_childs_conversation_is_not_in_the_persons_list_of_conversations() {
    let step = Arc::new(AtomicUsize::new(0));
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(Scripted {
            step: step.clone(),
            bodies: vec![
                tool_call("task", r#"{"prompt":"what is in the box?"}"#),
                prose("CHILD FOUND THE ANSWER"),
                prose("PARENT DONE"),
            ],
        })
        .mount(&server)
        .await;

    let home = scratch("children", &server.uri());
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("work dir");
    let (code, stdout, stderr) = run_flint(&home, &work, &[]);
    assert_eq!(code, 0, "flint failed: {stderr}");

    let parent = session_of(&stdout);
    let child = sessions_named(&transcript(&parent))
        .into_iter()
        .next()
        .expect("the result names the child's session");
    assert_eq!(
        child
            .parent()
            .and_then(|dir| dir.file_name())
            .and_then(|name| name.to_str()),
        Some("children"),
        "the child's conversation is not filed where the listing cannot reach it: {}",
        child.display()
    );
    // And the file says so itself, for anyone reading it by hand.
    let raw = std::fs::read_to_string(&child).expect("read the child's session");
    let meta = raw.lines().next().expect("a meta line");
    let parent_id = parent
        .file_stem()
        .expect("the parent's id")
        .to_string_lossy()
        .to_string();
    assert!(
        meta.contains(&format!(r#""parent":"{parent_id}""#)),
        "the child's record does not name the conversation that asked for it: {meta}"
    );

    // The person's list, as a program reads it: one row, and it is theirs.
    let listing = binary()
        .args(["--list-sessions", "--json"])
        .env("FLINT_HOME", &home)
        .output()
        .expect("failed to list sessions");
    let listing = String::from_utf8_lossy(&listing.stdout).to_string();
    let parsed: serde_json::Value =
        serde_json::from_str(&listing).expect("the listing is one JSON object");
    let rows = parsed["sessions"].as_array().expect("a sessions array");
    assert_eq!(
        rows.len(),
        1,
        "the person's list is not just the person's conversations: {listing}"
    );
    assert_eq!(rows[0]["id"].as_str(), Some(parent_id.as_str()));

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

#[tokio::test]
async fn a_profile_is_what_decides_the_instructions_the_model_and_readonly() {
    let step = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(Watched {
            step: step.clone(),
            seen: seen.clone(),
            delay: std::time::Duration::ZERO,
            bodies: vec![
                // The parent names the profile, and waits: the point here is what the profile did to
                // the child, which is only visible in the child's own turn.
                tool_call(
                    "task",
                    r#"{"prompt":"look around","agent":"explorer","background":false}"#,
                ),
                // The child, being readonly, is asked to write -- which is the only way to see from
                // outside that the profile's `readonly: true` arrived.
                tool_call("write", r#"{"path":"child.txt","content":"should not exist"}"#),
                prose("CHILD LOOKED"),
                prose("PARENT DONE"),
            ],
        })
        .mount(&server)
        .await;

    let home = scratch("profile", &server.uri());
    let work = home.join("work");
    std::fs::create_dir_all(work.join(".flint/agents")).expect("agents dir");
    std::fs::write(
        work.join(".flint/agents/explorer.md"),
        "---\nname: explorer\ndescription: Reads and reports.\nmodel: profile-model\nreadonly: true\n---\n\n\
         PROFILE INSTRUCTIONS: you only read, and you report what you found.\n",
    )
    .expect("profile");

    let (code, stdout, stderr) = run_flint(&home, &work, &[]);
    assert_eq!(code, 0, "flint failed: {stderr}");
    let text = transcript(&session_of(&stdout));

    let requests = seen.lock().expect("seen lock").clone();
    assert_eq!(requests.len(), 4, "expected four requests, got {}", requests.len());
    // Request 0 is the parent's, request 1 is the child's: two processes, one stub, and order is the
    // only thing that tells them apart.
    let parent_asked = &requests[0].0;
    let child_asked = &requests[1].0;
    assert!(
        child_asked.contains("PROFILE INSTRUCTIONS"),
        "the profile's instructions never reached the child: {child_asked}"
    );
    assert!(
        child_asked.contains("look around"),
        "the job itself never reached the child: {child_asked}"
    );
    assert!(
        child_asked.contains("\"model\":\"profile-model\""),
        "the profile's model was not used for the child: {child_asked}"
    );
    assert!(
        !parent_asked.contains("PROFILE INSTRUCTIONS"),
        "the profile's instructions leaked into this run's own request: {parent_asked}"
    );

    assert!(
        !work.join("child.txt").exists(),
        "the profile said readonly and the child wrote anyway"
    );
    assert!(text.contains("readonly: true"), "{text}");
    assert!(text.contains("CHILD LOOKED"), "{text}");

    let _ = std::fs::remove_dir_all(&home);
}

/// A provider for a fan-out: the parent is scripted, and every other request is answered *from its
/// own prompt*.
///
/// That is the whole trick of this responder, and the reason it exists. Three children start at the
/// same moment, so the order their requests arrive is not the order the jobs were listed in -- a stub
/// that answered by request number would be asserting a mapping that does not exist. Answering "job
/// two" with "ANSWER TWO" makes the pairing checkable from both ends: the block that says `task 2`
/// must hold the answer that only job two could have provoked.
struct Jobs {
    step: Arc<AtomicUsize>,
    seen: Arc<std::sync::Mutex<Vec<(String, std::time::Instant)>>>,
    delay: std::time::Duration,
}

const JOBS: [(&str, &str); 3] = [
    ("job one", "ANSWER ONE"),
    ("job two", "ANSWER TWO"),
    ("job three", "ANSWER THREE"),
];

impl Respond for Jobs {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let body = String::from_utf8_lossy(&req.body).to_string();
        self.seen
            .lock()
            .expect("seen lock")
            .push((body.clone(), std::time::Instant::now()));
        let n = self.step.fetch_add(1, Ordering::SeqCst);

        let answer = if n == 0 {
            // The parent, asking for the fan-out.
            tool_call(
                "tasks",
                r#"{"tasks":[{"prompt":"job one"},{"prompt":"job two"},{"prompt":"job three"}]}"#,
            )
        } else if let Some((_, answer)) = JOBS.iter().find(|(prompt, _)| body.contains(prompt)) {
            // A child, held for a while -- asynchronously, through the response rather than by
            // sleeping in this handler. A handler that slept would hold the stub's single thread and
            // serialize the very requests this test exists to time, which is exactly what happened
            // the first time this test was written.
            return ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_delay(self.delay)
                .set_body_string(prose(answer));
        } else {
            // The parent, having read the tool result.
            prose("PARENT DONE")
        };
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string(answer)
    }
}

/// One at a time, for the tests that read a wall clock.
///
/// Cargo runs the tests in a binary on several threads, and each of these starts real flint processes
/// against its own stub: a fan-out of three children measured while three other tests are starting
/// processes is measuring the machine, not the tool. The file's other tests are about content and do
/// not care.
static ALONE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn alone() -> tokio::sync::MutexGuard<'static, ()> {
    ALONE.lock().await
}

/// How long the stub holds each child's answer in the fan-out test.
///
/// This is the test's *unit* of time, and both bounds below are written in terms of it rather than in
/// milliseconds, because an absolute bound measures the machine as much as the tool: three seconds
/// here means a serial fan-out cannot finish in less than 9 s, which leaves room for the process
/// starts a slow runner adds. The first version of this used 2 s with a 5 s wall-clock bound and went
/// red on the CI runner at 5.078 s -- the tool was right and the bound was a coin flip. See the
/// per-step check in `.github/workflows/ci.yml`, which is how that number was read.
const JOBS_DELAY: std::time::Duration = std::time::Duration::from_secs(3);

#[tokio::test]
async fn a_fan_out_runs_the_jobs_at_the_same_time_and_labels_every_answer() {
    let _solo = alone().await;
    let step = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(Jobs {
            step: step.clone(),
            seen: seen.clone(),
            delay: JOBS_DELAY,
        })
        .mount(&server)
        .await;

    let home = scratch("fanout", &server.uri());
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("work dir");

    let started = std::time::Instant::now();
    let (code, stdout, stderr) = run_flint(&home, &work, &[]);
    let elapsed = started.elapsed();
    assert_eq!(code, 0, "flint failed: {stderr}");
    let text = transcript(&session_of(&stdout));

    // Every answer is there, under the job it belongs to. Split on the headers rather than searching
    // the whole text, because "the right answer came back" is not the same as "the right answer came
    // back under the right question".
    let blocks: Vec<&str> = text.split("--- task ").skip(1).collect();
    assert_eq!(blocks.len(), 3, "expected three blocks: {text}");
    for (index, (block, answer)) in blocks
        .iter()
        .zip(["ANSWER ONE", "ANSWER TWO", "ANSWER THREE"])
        .enumerate()
    {
        assert!(
            block.starts_with(&format!("{} ---", index + 1)),
            "block {index} is not labelled {}: {block}",
            index + 1
        );
        assert!(
            block.contains(answer),
            "block {} does not hold {answer}, so the answers are not in the order the jobs were \
             asked for: {block}",
            index + 1
        );
        assert!(
            block.contains("session: "),
            "block {} names no session, so its answer cannot be traced: {block}",
            index + 1
        );
    }
    assert!(
        text.contains("3 children ran,"),
        "the header does not say what ran: {text}"
    );

    // When each request arrived. Nothing else in this test can see whether the children overlapped.
    let requests = seen.lock().expect("seen lock").clone();
    assert_eq!(requests.len(), 5, "expected five requests, got {}", requests.len());
    let child_arrivals: Vec<std::time::Instant> = requests
        .iter()
        // Exactly one job prompt: a child's request holds the job it was asked, while the parent's
        // follow-up turn holds all three inside the tool call it made, and counting that one would
        // make this a count of four children.
        .filter(|(body, _)| {
            JOBS.iter()
                .filter(|(prompt, _)| body.contains(prompt))
                .count()
                == 1
        })
        .map(|(_, at)| *at)
        .collect();
    assert_eq!(child_arrivals.len(), 3, "the children's requests were not found");
    let spread = child_arrivals
        .iter()
        .max()
        .expect("three arrivals")
        .duration_since(*child_arrivals.iter().min().expect("three arrivals"));
    // A third of the hold, because the two things being told apart are one hold apart: three
    // children one after another leave that much between arrivals, and a third of it is more room
    // than a slow machine needs to start three processes.
    assert!(
        spread < JOBS_DELAY / 3,
        "the children did not start together: their requests arrived {spread:?} apart, and three \
         children one after another would be about {JOBS_DELAY:?} apart"
    );
    // And the whole run beats the serial floor *by construction*: three children in a row cannot
    // finish before 3 x the hold, whatever the machine does to process starts, because each answer
    // waits out the hold in the stub. A tighter bound than that would be measuring the runner.
    assert!(
        elapsed < 3 * JOBS_DELAY,
        "the fan-out did not overlap: it took {elapsed:?}, and three children one after another \
         would take at least {:?}",
        3 * JOBS_DELAY
    );

    let _ = std::fs::remove_dir_all(&home);
}

#[tokio::test]
async fn a_fan_out_that_is_already_deep_is_refused_before_any_child_starts() {
    let step = Arc::new(AtomicUsize::new(0));
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(Scripted {
            step: step.clone(),
            bodies: vec![
                tool_call("tasks", r#"{"tasks":[{"prompt":"one"},{"prompt":"two"}]}"#),
                prose("PARENT DONE"),
            ],
        })
        .mount(&server)
        .await;

    let home = scratch("fanout-depth", &server.uri());
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("work dir");

    let out = binary()
        .args([
            "-p",
            "ask the children",
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

    assert!(text.contains("does not go deeper than 2"), "{text}");
    assert_eq!(
        step.load(Ordering::SeqCst),
        2,
        "two children were started despite the depth limit"
    );

    let _ = std::fs::remove_dir_all(&home);
}

/// Answers by request number, holding every answer but the first for a while.
///
/// The first is the parent's, and it starts the child; every later one is the child's, and the delay is
/// what makes the parent wait long enough for its status row to have been painted at all -- the row is
/// held back for [`ACTIVITY_DELAY`] so that a fast tool does not flicker a clock nobody can read.
struct SlowAfterFirst {
    step: Arc<AtomicUsize>,
    bodies: Vec<String>,
    delay: std::time::Duration,
}

impl Respond for SlowAfterFirst {
    fn respond(&self, _req: &Request) -> ResponseTemplate {
        let n = self.step.fetch_add(1, Ordering::SeqCst);
        let body = self
            .bodies
            .get(n)
            .or_else(|| self.bodies.last())
            .cloned()
            .unwrap_or_else(|| prose("NOTHING SCRIPTED"));
        let template = ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string(body);
        if n == 0 {
            template
        } else {
            template.set_delay(self.delay)
        }
    }
}

/// A parent asks for a child; the child is then held, so it is still running when the test looks.
///
/// Only the child's request is delayed: the parent's second turn has to be answered, or a test waits
/// for a conversation that never continues.
struct HeldChild {
    step: Arc<AtomicUsize>,
    hold: std::time::Duration,
    /// What the parent's later turns are told, so a test can see the session end normally.
    then: &'static str,
}

impl Respond for HeldChild {
    fn respond(&self, _req: &Request) -> ResponseTemplate {
        let n = self.step.fetch_add(1, Ordering::SeqCst);
        let body = match n {
            // Waiting, because this test drops the *wait*: the parent is inside `task` when the person
            // types, which is the reported incident, and a background child would have returned already.
            0 => tool_call("task", r#"{"prompt":"look around","background":false}"#),
            1 => prose("CHILD EVENTUALLY ANSWERS"),
            _ => prose(self.then),
        };
        let template = ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string(body);
        if n == 1 {
            template.set_delay(self.hold)
        } else {
            template
        }
    }
}

/// A parent's own account of the child it left running, and the child's session path inside it.
fn left_running_in(text: &str) -> Option<(u32, String)> {
    let pid = pid_from(text)?;
    let session = session_path_from(text)?;
    Some((pid, session))
}

/// A line typed while a child is running drops the turn -- and the parent must say what it left going.
///
/// This is the reported bug, reproduced: a `task` child is started, the person (seeing nothing change,
/// because nothing did) types at the parent, and the turn is dropped. What the parent recorded was
/// "tool 'task' was requested but never ran", which is false in both halves -- it ran, and it was still
/// running minutes later with its own bill -- and a model reading that sentence offers to run the task
/// again, spending the same money twice.
///
/// The REPL is driven through its own input path rather than with `-p`: the second line has to arrive
/// *while* the first turn is waiting, which is what steering is, and a one-shot run has only one turn
/// to be interrupted in. Under `FLINT_TERM_CAPTURE` the input comes from the pipe instead of a
/// keyboard (see `main`), which is what makes a whole session scriptable from a test.
#[tokio::test]
async fn an_interrupted_task_says_what_it_left_running() {
    let _solo = alone().await;
    let step = Arc::new(AtomicUsize::new(0));
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(HeldChild {
            step: step.clone(),
            hold: std::time::Duration::from_secs(120),
            then: "PARENT DONE",
        })
        .mount(&server)
        .await;

    let home = scratch("interrupted", &server.uri());
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("work dir");
    let capture = std::env::temp_dir().join(format!(
        "flint-task-interrupted-{}.cap",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&capture);

    let mut child = binary()
        .args(["--cwd", &work.display().to_string()])
        .env("FLINT_HOME", &home)
        .env_remove("FLINT_DEPTH")
        .env("FLINT_TERM_CAPTURE", "1")
        .env("FLINT_TERM_CAPTURE_FILE", &capture)
        .env("FLINT_TERM_SIZE", "100x24")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("failed to run flint");
    {
        use std::io::Write;
        let stdin = child.stdin.as_mut().expect("stdin");
        stdin.write_all(b"ask the child\n").expect("write");
        stdin.flush().expect("flush");
    }
    // Long enough for the child to have started -- its own request is what the parent is waiting on --
    // and short enough that it is still waiting when the line arrives.
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    {
        use std::io::Write;
        let stdin = child.stdin.as_mut().expect("stdin");
        stdin.write_all("any news?\n".as_bytes()).expect("write");
        stdin.flush().expect("flush");
    }
    drop(child.stdin.take());
    let status = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        loop {
            if let Some(status) = child.try_wait().expect("wait") {
                return status;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the parent never exited");
    assert_eq!(status.code(), Some(0), "the session ended badly");
    let _ = std::fs::remove_file(&capture);

    // The parent's own session, found by what only it says: both of its prompts.
    let mut parent = None;
    for entry in walk(&home.join("sessions")) {
        let text = transcript(&entry);
        if text.contains("ask the child") && text.contains("any news?") {
            parent = Some(text);
        }
    }
    let text = parent.expect("the parent's session was not found");
    assert!(
        text.contains("the result of 'task' never came back"),
        "the record still claims the tool never ran: {text}"
    );
    assert!(
        text.contains("still going on its own"),
        "nothing says the child outlived the turn: {text}"
    );
    // The child's session path is the actionable half: the answer is being written there, and a run
    // that says so is a run whose work can still be collected.
    let (pid, child_session) =
        left_running_in(&text).expect("the note does not say what it left running");
    assert!(
        std::path::Path::new(&child_session).is_file(),
        "the note names a session that is not there: {child_session}"
    );
    assert!(
        transcript(std::path::Path::new(&child_session)).contains("look around"),
        "the session in the note is not the child's: {child_session}"
    );

    // Left behind on purpose, and stopped here: a test that walked away from it would leave a flint
    // running on this machine, which is the fault being fixed rather than a way to end a test.
    stop(pid);
    let _ = std::fs::remove_dir_all(&home);
}

/// A `--json` run has no next turn, so the fact has to be on the stream or it is nowhere.
///
/// This is the caller-shaped half of the same bug: a program that ran flint with a budget gets exit 65
/// and, without this, no way at all to learn that a child it paid for is still going and where its
/// answer will be. The parent's own session is not the caller's to read -- `close_dangling_tool_calls`
/// only runs on a *next* turn, which a one-shot does not have -- so the warning is the only channel
/// left, and it is emitted before the outcome line because it is part of how the run ended.
#[tokio::test]
async fn a_json_run_cut_short_names_the_child_it_left_running() {
    let _solo = alone().await;
    let step = Arc::new(AtomicUsize::new(0));
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(HeldChild {
            step: step.clone(),
            hold: std::time::Duration::from_secs(120),
            then: "PARENT DONE",
        })
        .mount(&server)
        .await;

    let home = scratch("cut-short", &server.uri());
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("work dir");
    let out_file = home.join("stream.ndjson");
    let mut child = binary()
        .args([
            "-p",
            "ask the child",
            "--json",
            "--max-seconds",
            "2",
            "--cwd",
            &work.display().to_string(),
        ])
        .env("FLINT_HOME", &home)
        .env_remove("FLINT_DEPTH")
        .stdin(std::process::Stdio::null())
        // A file rather than `output()`, and not for tidiness: the child is started with its own
        // pipes, but a *grandchild* still holds whatever this end of the stream is open with until it
        // exits, so reading a pipe to EOF here would block for as long as the child this test exists
        // to catch outlives its parent. Reading the file once the parent is gone is the same bytes
        // without that.
        .stdout(std::process::Stdio::from(
            std::fs::File::create(&out_file).expect("stream file"),
        ))
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("failed to run flint");
    let started = std::time::Instant::now();
    let status = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        loop {
            if let Some(status) = child.try_wait().expect("wait") {
                return status;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the parent never exited");
    let took = started.elapsed();
    let stdout = std::fs::read_to_string(&out_file).expect("stream file");
    assert_eq!(
        status.code(),
        Some(65),
        "not the budget's exit code: {stdout}"
    );
    assert!(
        took < std::time::Duration::from_secs(30),
        "the run outlived its own budget by {took:?}: the deadline did not cut it"
    );

    let warnings: Vec<String> = stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|frame| frame["type"] == "warning")
        .filter_map(|frame| frame["message"].as_str().map(str::to_string))
        .collect();
    let note = warnings
        .iter()
        .find(|text| text.contains("still going on its own"))
        .unwrap_or_else(|| panic!("the caller was never told about the child: {warnings:?}"));
    let (pid, child_session) = left_running_in(note).expect("the warning names nothing actionable");
    assert!(
        Path::new(&child_session).is_file(),
        "the warning names a session that is not there: {child_session}"
    );
    // The outcome still comes, and still says what it always said: the child is an addition to the
    // caller's picture, not a change to the vocabulary.
    assert!(stdout.contains(r#""outcome":"incomplete""#), "{stdout}");
    assert!(stdout.contains(r#""reason":"seconds""#), "{stdout}");

    stop(pid);
    let _ = std::fs::remove_dir_all(&home);
}

/// Every file under a directory, one level of `read_dir` at a time.
fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(walk(&path));
        } else {
            found.push(path);
        }
    }
    found
}

/// The pid in the sentence a dropped turn leaves about its child.
fn pid_from(text: &str) -> Option<u32> {
    let at = text.find("(pid ")? + "(pid ".len();
    let digits: String = text[at..].chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// The session path in the same sentence.
fn session_path_from(text: &str) -> Option<String> {
    let at = text.find("own session, ")? + "own session, ".len();
    let rest = &text[at..];
    let end = rest.find(" --")?;
    Some(rest[..end].trim().to_string())
}

/// End a process this test started and deliberately left running.
fn stop(pid: u32) {
    #[cfg(windows)]
    let mut command = {
        let mut c = std::process::Command::new("taskkill");
        c.args(["/PID", &pid.to_string(), "/T", "/F"]);
        c
    };
    #[cfg(not(windows))]
    let mut command = {
        let mut c = std::process::Command::new("kill");
        c.args(["-9", &pid.to_string()]);
        c
    };
    let _ = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// While the parent waits for a child, the child's own work shows on the parent's status row.
///
/// This is a bug report, not a nicety. A realistic `task` runs for minutes -- a research child made
/// eleven model calls and took two -- and the parent's row says one unchanging word for all of it:
/// `task`. Someone watching that cannot tell a child that is working from one that is stuck, and the
/// reported outcome was exactly that: "no further changes, the indicator is gone, I do not know
/// whether it is running" -- followed by typing at it, which (because a typed line steers) dropped
/// the turn and discarded the child's answer. The child is already saying what it is doing on its own
/// `--json` stream; the parent was reading those frames and throwing them away.
///
/// The capture harness is how a test can see the status row at all: it draws the interactive layout
/// into a file, and `-p` plus `FLINT_TERM_CAPTURE` takes the interactive path on purpose (see
/// `term::capture_requested`).
#[tokio::test]
async fn a_childs_own_progress_reaches_the_parents_status_row() {
    let _solo = alone().await;
    let step = Arc::new(AtomicUsize::new(0));
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(SlowAfterFirst {
            step: step.clone(),
            delay: std::time::Duration::from_secs(2),
            bodies: vec![
                // Waiting: a status row about a child is only drawn while the parent is there to draw
                // it, and this test is about what the row says while the child works.
                tool_call("task", r#"{"prompt":"look around","background":false}"#),
                // The child asks for a listing, which is the frame the parent has to pass on.
                tool_call("list", r#"{"path":"."}"#),
                // The child answers...
                prose("CHILD DONE"),
                // ...and the parent finishes.
                prose("PARENT DONE"),
            ],
        })
        .mount(&server)
        .await;

    let home = scratch("progress", &server.uri());
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("work dir");
    let capture = std::env::temp_dir().join(format!(
        "flint-task-progress-{}.cap",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&capture);

    let out = binary()
        .args(["-p", "ask the child", "--cwd", &work.display().to_string()])
        .env("FLINT_HOME", &home)
        .env("FLINT_TERM_CAPTURE", "1")
        .env("FLINT_TERM_CAPTURE_FILE", &capture)
        .env("FLINT_TERM_SIZE", "100x24")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("failed to run flint");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    // A captured run draws into the file, and that is where the answer and the status row both are.
    let drawn = std::fs::read(&capture)
        .map(|bytes| String::from_utf8_lossy(&bytes).to_string())
        .unwrap_or_default();
    let _ = std::fs::remove_file(&capture);
    let _ = std::fs::remove_dir_all(&home);

    assert_eq!(
        out.status.code(),
        Some(0),
        "flint failed: {stdout} / {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        drawn.contains("PARENT DONE"),
        "the parent never drew its answer, so the capture is not a run: {drawn:?}"
    );
    // Named for the child, not for a tool of the parent's: "list" is the child's tool, and the
    // parent has no such call, so this text can only have come from the child's stream.
    assert!(
        drawn.contains("task: running list"),
        "the parent's status row never said what the child was doing: {drawn:?}"
    );
}

/// One step of a scripted conversation.
enum Step {
    /// Answer with prose.
    Say(&'static str),
    /// Ask for a tool. `{pid}` is replaced by the child's pid, read out of the request: a model knows
    /// which child it is talking about only because the tool result told it.
    Call(&'static str, &'static str),
}

/// A scripted model that answers by *conversation* rather than by request number.
///
/// A background child asks the model whenever it likes, so "the parent's turn is request 3" stops
/// being true the moment nothing waits for the child -- the child's request races the parent's next
/// one. The first user message is what tells two conversations apart, and each conversation keeps its
/// own count.
struct Scripts {
    steps: Arc<std::sync::Mutex<std::collections::HashMap<String, usize>>>,
    /// (a phrase from that conversation's first user message, its steps, how long to hold each answer)
    scripts: Vec<(&'static str, Vec<Step>, std::time::Duration)>,
    /// Every request body that arrived, in order, whichever conversation sent it.
    ///
    /// The only place a *view* is observable. A note flint adds to a request is not in the session
    /// file by construction -- that is what makes it a view -- so "the model was told" can only be
    /// asserted against the bytes the stub received.
    seen: Arc<std::sync::Mutex<Vec<String>>>,
}

/// A `Scripts` responder, built by name so a test can also hold the list of request bodies.
impl Scripts {
    fn new(scripts: Vec<(&'static str, Vec<Step>, std::time::Duration)>) -> Scripts {
        Scripts {
            steps: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            scripts,
            seen: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    /// The request bodies that have arrived so far, in order.
    fn seen(&self) -> Arc<std::sync::Mutex<Vec<String>>> {
        Arc::clone(&self.seen)
    }
}

impl Respond for Scripts {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let body = String::from_utf8_lossy(&req.body).to_string();
        self.seen
            .lock()
            .expect("seen lock")
            .push(body.clone());
        let parsed: serde_json::Value =
            serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
        let first = parsed
            .get("messages")
            .and_then(|messages| messages.as_array())
            .and_then(|messages| {
                messages
                    .iter()
                    .find(|message| message.get("role").and_then(|r| r.as_str()) == Some("user"))
            })
            .and_then(|message| message.get("content").and_then(|c| c.as_str()))
            .unwrap_or("")
            .to_string();
        let (phrase, steps, delay) = self
            .scripts
            .iter()
            .find(|(phrase, ..)| first.contains(phrase))
            .unwrap_or_else(|| panic!("no script for a conversation that began with: {first}"));
        let n = {
            let mut counts = self.steps.lock().expect("steps lock");
            let count = counts.entry((*phrase).to_string()).or_insert(0);
            let n = *count;
            *count += 1;
            n
        };
        let answer = match steps.get(n).or_else(|| steps.last()) {
            Some(Step::Say(text)) => prose(text),
            Some(Step::Call(tool, args)) => {
                let args = if args.contains("{pid}") {
                    let pid = pid_from_a_handle(&body).unwrap_or_else(|| {
                        panic!("{tool} was given a pid placeholder and the request has no handle")
                    });
                    args.replace("{pid}", &pid.to_string())
                } else {
                    (*args).to_string()
                };
                tool_call(tool, &args)
            }
            None => prose("NOTHING SCRIPTED"),
        };
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_delay(*delay)
            .set_body_string(answer)
    }
}

/// The pid in the handle a background `task` hands back, which is the only place a script can get it.
fn pid_from_a_handle(text: &str) -> Option<u32> {
    let at = text.find("background: pid ")? + "background: pid ".len();
    let digits: String = text[at..].chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Run a flint and report when *it* exited, which is not the same moment its pipes close.
///
/// `run_flint` uses `output()`, and that waits for the last writer of stdout to go away -- which is
/// the parent only while nothing it started holds the pipe. A background child is exactly the case
/// where the two differ, so the test that is about "the parent did not wait" measures the parent's own
/// exit rather than the harness's patience. Nothing is read from the streams here: the session file is
/// where the test looks, as it does everywhere else.
fn run_flint_until_exit(home: &Path, work: &Path) -> (i32, std::time::Duration) {
    let mut child = binary()
        .args([
            "-p",
            "ask the child",
            "--json",
            "--cwd",
            &work.display().to_string(),
        ])
        .env("FLINT_HOME", home)
        .env_remove("FLINT_DEPTH")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("failed to run flint");
    let clock = std::time::Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("wait") {
            return (status.code().unwrap_or(-1), clock.elapsed());
        }
        if clock.elapsed() > std::time::Duration::from_secs(90) {
            let _ = child.kill();
            panic!("the parent never exited");
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

/// A child is a handle *by default*: the parent finishes while the child works, and the answer is on
/// disk afterwards.
///
/// The scripted model says nothing about waiting, so this is the default path and not the opt-in one.
/// It is the right default for a child because a child is a whole run: its answer is being written to
/// a conversation of its own whether or not anybody waits, so waiting buys nothing the file does not
/// already hold -- and it costs the person the use of their session for as long as the child takes.
/// A model that needs the answer before it can continue says `background: false`, and the test below
/// this one is that door. The alternative an agent had was `bash` with an ampersand, which loses the
/// exit code, the session path and the stream.
///
/// The three facts checked here are the ones a caller cannot reconstruct: the parent did not wait (the
/// clock), the handle names the conversation (the session path), and the answer lands there after the
/// parent is gone (the file).
#[tokio::test]
async fn a_background_child_is_a_handle_and_answers_after_the_parent_is_gone() {
    let _solo = alone().await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(Scripts::new(vec![
            (
                "ask the child",
                vec![
                    // The parent starts a job and does not wait for it -- by saying nothing about
                    // waiting, which is the default.
                    Step::Call("task", r#"{"prompt":"SLOW JOB"}"#),
                    Step::Say("PARENT DONE"),
                ],
                std::time::Duration::ZERO,
            ),
            // Eight seconds of work, against a parent whose own run takes one.
            (
                "SLOW JOB",
                vec![Step::Say("CHILD WAS SLOW")],
                std::time::Duration::from_secs(8),
            ),
        ]))
        .mount(&server)
        .await;

    let home = scratch("background", &server.uri());
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("work dir");

    let (code, elapsed) = run_flint_until_exit(&home, &work);
    assert_eq!(code, 0, "the parent ended badly");
    // The child's answer is eight seconds away and the parent is done in one: a parent that had waited
    // would be past this by the time it wrote its answer, which is what the clock says.
    assert!(
        elapsed < std::time::Duration::from_secs(6),
        "the parent waited for a background child: {elapsed:?}"
    );

    // The parent's own conversation, found by what only it says: its prompt, and not a child's.
    let mut parent = None;
    for entry in walk(&home.join("sessions")) {
        if entry.to_string_lossy().contains("children") {
            continue;
        }
        let text = transcript(&entry);
        if text.contains("ask the child") {
            parent = Some(text);
        }
    }
    let text = parent.expect("the parent's session was not found");
    assert!(
        text.contains("started in the background: pid "),
        "no handle came back: {text}"
    );

    let child_session = sessions_named(&text)
        .into_iter()
        .find(|path| path.to_string_lossy().contains("children"))
        .expect("the handle did not name the child's conversation");
    assert!(
        child_session.is_file(),
        "the handle names a session that is not there: {child_session:?}"
    );

    // The child outlived the parent, and its answer is where the handle said it would be. Polled
    // rather than read once: the point of a background child is that nobody is waiting for it, so the
    // test has to be the one that waits.
    let mut answer = String::new();
    for _ in 0..120 {
        answer = transcript(&child_session);
        if answer.contains("CHILD WAS SLOW") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    assert!(
        answer.contains("CHILD WAS SLOW"),
        "the child never wrote its answer after the parent exited: {answer}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// A handle is usable: a model can ask where a child is, and then collect what it said.
///
/// The pid is not something the scripted model could know -- it arrives in the tool result -- so the
/// stub reads it back out of the request, which is exactly what a model does. That is the property
/// under test: the handle is not a receipt to look at, it is an argument for the next call.
#[tokio::test]
async fn a_handle_says_where_a_child_is_and_then_collects_its_answer() {
    let _solo = alone().await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(Scripts::new(vec![
            (
                "ask the child",
                vec![
                    Step::Call("task", r#"{"prompt":"SLOW JOB"}"#),
                    Step::Call("job_op", r#"{"action":"status"}"#),
                    Step::Call("job_op", r#"{"action":"wait","pid":{pid}}"#),
                    Step::Say("PARENT DONE"),
                ],
                std::time::Duration::ZERO,
            ),
            // Long enough that the status step sees it running and the wait step has to wait.
            (
                "SLOW JOB",
                vec![Step::Say("THE SLOW ANSWER")],
                std::time::Duration::from_secs(3),
            ),
        ]))
        .mount(&server)
        .await;

    let home = scratch("handle", &server.uri());
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("work dir");

    let (code, stdout, stderr) = run_flint(&home, &work, &[]);
    assert_eq!(code, 0, "flint failed: {stderr}\n{stdout}");
    let session = session_of(&stdout);
    let text = transcript(&session);
    assert!(
        text.contains("PARENT DONE"),
        "the parent did not finish: {stdout}"
    );
    // The status answer: where it is, and which job it is -- a pid alone would not tell a model that.
    assert!(
        text.contains("running for") && text.contains("asked: SLOW JOB"),
        "the status answer does not say where the child is: {text}"
    );
    // And the wait answer, in the same shape a foreground `task` gives: the child's words, and the
    // facts that decide what to do with them.
    assert!(
        text.contains("THE SLOW ANSWER"),
        "the wait never gave back the child's answer: {text}"
    );
    assert!(
        text.contains("exit code: 0 (finished)"),
        "the wait gave back an answer with no exit code: {text}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// A background child of a run that keeps nothing says so in every place that would otherwise
/// promise a file: the handle, the status line, and the answer a `wait` hands over.
///
/// The words are the point here rather than the mechanism. "its session is not named yet" is an
/// answer a caller keeps asking about -- a status line that stays wrong forever, and a `job_op` call
/// per turn to learn nothing -- so each of the three has to tell "not yet" from "started with
/// `--no-session`, so there is none coming".
#[tokio::test]
async fn a_background_child_of_a_run_that_keeps_nothing_says_so_everywhere() {
    let _solo = alone().await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(Scripts::new(vec![
            (
                "ask the child",
                vec![
                    Step::Call("task", r#"{"prompt":"SLOW JOB"}"#),
                    Step::Call("job_op", r#"{"action":"status"}"#),
                    Step::Call("job_op", r#"{"action":"wait","pid":{pid}}"#),
                    Step::Say("PARENT DONE"),
                ],
                std::time::Duration::ZERO,
            ),
            (
                "SLOW JOB",
                vec![Step::Say("THE SLOW ANSWER")],
                std::time::Duration::from_secs(3),
            ),
        ]))
        .mount(&server)
        .await;

    let home = scratch("keeps-nothing-background", &server.uri());
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("work dir");
    let (code, stdout, stderr) = run_flint(&home, &work, &["--no-session"]);
    assert_eq!(code, 0, "flint failed: {stderr}\n{stdout}");

    assert!(
        stdout.contains("session: none -- it was started with --no-session"),
        "the handle does not say the child keeps nothing: {stdout}"
    );
    assert!(
        stdout.contains("it keeps no conversation (--no-session)"),
        "the status line reads as though a session were still coming: {stdout}"
    );
    assert!(
        stdout.contains("THE SLOW ANSWER")
            && stdout.contains("session: none (--no-session)"),
        "the answer a wait hands over does not say the same: {stdout}"
    );
    let files = session_files(&home);
    assert!(
        files.is_empty(),
        "a background child wrote a conversation its parent was told not to keep: {files:?}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// A child that is going nowhere is asked to stop, and stops -- the one verb here that changes
/// anything.
///
/// The word matters. `/stop` is what a person types at a run and what the timeout path already writes,
/// so a child that hears it keeps the half of an answer it had drawn and exits 130; a kill would throw
/// that away. This is also why the child's stdin lives in the registry rather than inside the future
/// that spawned it: by the time a model asks a child to stop, that future is a returned tool result.
#[tokio::test]
async fn a_handle_can_ask_a_stuck_child_to_stop() {
    let _solo = alone().await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(Scripts::new(vec![
            (
                "ask the child",
                vec![
                    Step::Call("task", r#"{"prompt":"STUCK JOB"}"#),
                    Step::Call("job_op", r#"{"action":"stop","pid":{pid}}"#),
                    Step::Say("PARENT DONE"),
                ],
                std::time::Duration::ZERO,
            ),
            // A model call that never comes back, which is the state a stop is for.
            (
                "STUCK JOB",
                vec![Step::Say("NEVER ANSWERED")],
                std::time::Duration::from_secs(60),
            ),
        ]))
        .mount(&server)
        .await;

    let home = scratch("stop", &server.uri());
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("work dir");

    let (code, stdout, stderr) = run_flint(&home, &work, &[]);
    assert_eq!(code, 0, "flint failed: {stderr}\n{stdout}");
    let session = session_of(&stdout);
    let text = transcript(&session);
    assert!(
        text.contains("PARENT DONE"),
        "the parent did not finish: {stdout}"
    );
    assert!(
        text.contains("asked to stop"),
        "the stop said nothing about a child: {text}"
    );
    // 130 is "the run was stopped", and it is the child's own code: a child that had been killed would
    // have none, and one that ran into its timeout would say so instead.
    assert!(
        text.contains("exit code: 130 (the run was stopped)"),
        "the child did not stop the way a person stops one: {text}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// Waiting is a decision, and this is where a model makes it: `background: false` and the answer comes
/// back in the tool result, exactly as a call with no flag used to behave.
///
/// The two doors have to stay distinguishable in what the model *reads*, not only in when it reads it:
/// a handle where an answer was expected is a model that goes on to invent the answer, and an answer
/// where a handle was expected is one that never learns the job is still going. So the handle's own
/// words are asserted absent.
#[tokio::test]
async fn a_model_that_needs_the_answer_asks_to_wait() {
    let _solo = alone().await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(Scripts::new(vec![
            (
                "ask the child",
                vec![
                    Step::Call("task", r#"{"prompt":"QUICK JOB","background":false}"#),
                    Step::Say("PARENT DONE"),
                ],
                std::time::Duration::ZERO,
            ),
            (
                "QUICK JOB",
                vec![Step::Say("THE QUICK ANSWER")],
                std::time::Duration::from_secs(1),
            ),
        ]))
        .mount(&server)
        .await;

    let home = scratch("waiting", &server.uri());
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("work dir");

    let (code, stdout, stderr) = run_flint(&home, &work, &[]);
    assert_eq!(code, 0, "flint failed: {stderr}\n{stdout}");
    let text = transcript(&session_of(&stdout));
    assert!(
        text.contains("THE QUICK ANSWER"),
        "a call that asked to wait came back without the answer: {text}"
    );
    assert!(
        !text.contains("started in the background"),
        "a call that asked to wait handed back a handle instead: {text}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// A job that ends while the model is still working is reported to it once, and the report is a *view*.
///
/// Without this the default is a trap: a model that starts a child and does not wait learns nothing
/// when it ends, so the only way to find out would be to poll -- and a model that has moved on has no
/// reason to. The report is injected into the next request and nowhere else, which is what keeps the
/// session file the record of what *happened*: a conversation resumed from it is not told again about
/// a job that ended days ago, and the note never becomes a message somebody appears to have sent.
///
/// The scripted model is deliberately slow (two seconds a turn), the child deliberately quick, and the
/// parent given two more turns to take: "once" is a claim about a request *after* the one that carried
/// the report, so a script that ends on that request could not tell once from every-time.
#[tokio::test]
async fn a_child_that_ends_while_its_parent_works_is_reported_to_it_once() {
    let _solo = alone().await;
    let server = MockServer::start().await;
    let parent = Scripts::new(vec![
        (
            "ask the child",
            vec![
                Step::Call("task", r#"{"prompt":"QUICK JOB"}"#),
                // Something for the parent to do while the child finishes. Cheap and portable: the
                // point is that a turn goes on happening, not what the command prints.
                Step::Call("bash", r#"{"command":"echo still here"}"#),
                Step::Call("bash", r#"{"command":"echo and here"}"#),
                Step::Say("PARENT DONE"),
            ],
            std::time::Duration::from_secs(2),
        ),
        (
            "QUICK JOB",
            vec![Step::Say("CHILD QUICK")],
            std::time::Duration::from_secs(1),
        ),
    ]);
    let seen = parent.seen();
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(parent)
        .mount(&server)
        .await;

    let home = scratch("reported", &server.uri());
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("work dir");

    let (code, stdout, stderr) = run_flint(&home, &work, &[]);
    assert_eq!(code, 0, "flint failed: {stderr}\n{stdout}");

    let bodies = seen.lock().expect("seen lock").clone();
    // The stub sees every conversation, so the parent's are picked out by its own first question.
    let parent_bodies: Vec<&String> = bodies
        .iter()
        .filter(|body| body.contains("ask the child"))
        .collect();
    let told: Vec<usize> = parent_bodies
        .iter()
        .enumerate()
        .filter(|(_, body)| body.contains("not from the person"))
        .map(|(index, _)| index)
        .collect();
    assert_eq!(
        told.len(),
        1,
        "the model was told {} times that the job ended, out of {} requests: {told:?}",
        told.len(),
        parent_bodies.len()
    );
    // Never the first: the job cannot have ended before the call that started it. And never twice:
    // the parent is given a turn after the one that carried the report, which is where a report that
    // repeated itself would show up.
    assert!(
        told[0] > 0 && told[0] < parent_bodies.len() - 1,
        "the report arrived in the parent's request {} of {}: {told:?}",
        told[0],
        parent_bodies.len()
    );
    let note = &parent_bodies[told[0]];
    assert!(
        note.contains("exit code"),
        "the report does not say the job ended: {note}"
    );

    // And it names the job the way the handle named it, which is the only way a model can act on it.
    let text = transcript(&session_of(&stdout));
    let pid = pid_from_a_handle(&text).expect("the handle names a pid");
    assert!(
        note.contains(&format!("pid {pid}")),
        "the report does not name the job it is about: {note}"
    );

    // The report is a view: the conversation on disk does not hold it, so a resumed run is not told
    // about it a second time and no reader mistakes it for something the person said.
    assert!(
        !text.contains("not from the person"),
        "the report was written into the session file: {text}"
    );
    // The person, though, is told the moment it happens -- they are the one who can decide what to do
    // about a job that ended. stderr is where a non-interactive run's notices go.
    assert!(
        stderr.contains("finished"),
        "the person was never told the job ended: {stderr}"
    );
    let _ = std::fs::remove_dir_all(&home);
}
