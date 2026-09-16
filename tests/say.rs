//! The mailbox: two runs in one directory, and the one rule that makes it safe to have.
//!
//! A peer's words are shown to the person and written to the session file, and they are **never** put
//! into a request. That is not a nicety: anything that can write a mailbox could otherwise steer the
//! tool loop of a process that has no permission layer, and this file is where that is checked against
//! the bytes a provider actually received rather than against the intention.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

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

fn prose(text: &str) -> String {
    sse(&[
        &format!(r#"data: {{"choices":[{{"delta":{{"content":"{text}"}}}}]}}"#),
        r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
        r#"data: {"choices":[],"usage":{"prompt_tokens":5,"completion_tokens":2}}"#,
        "data: [DONE]",
    ])
}

/// The provider, with two jobs: answer, and act like a peer that speaks while the run works.
///
/// Every request body is kept, because the assertion that matters is about what the model was *sent*,
/// and nothing else can prove it.
struct Talking {
    seen: Arc<Mutex<Vec<String>>>,
    /// Where `flint say` should be pointed, and whether it has spoken yet.
    home: PathBuf,
    work: PathBuf,
    said: Mutex<bool>,
}

impl Respond for Talking {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let body = String::from_utf8_lossy(&req.body).to_string();
        self.seen.lock().expect("seen lock").push(body);
        if !*self.said.lock().expect("said lock") {
            *self.said.lock().expect("said lock") = true;
            // A peer speaks through the real command, in another process, exactly as a person in the
            // next terminal would. Not by writing the file directly: the CLI is the interface.
            let out = std::process::Command::new(env!("CARGO_BIN_EXE_flint"))
                .args([
                    "say",
                    "please do not commit docs/sandbox.md; I am still writing it",
                    "--cwd",
                    &self.work.display().to_string(),
                ])
                .env("FLINT_HOME", &self.home)
                .output()
                .expect("flint say");
            assert!(
                out.status.success(),
                "`flint say` failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string(prose("ANSWER"))
    }
}

fn scratch(tag: &str, base_url: &str) -> (PathBuf, PathBuf) {
    let home = std::env::temp_dir().join(format!("flint-say-{tag}-{}", std::process::id()));
    let work = home.join("work");
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&work).expect("work dir");
    std::fs::write(
        home.join("config.toml"),
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
    (home, work)
}

/// The mailbox a directory uses, asked of the same function the code asks -- so the test knows the
/// path without the process-wide `FLINT_HOME` having to be set in the test itself.
fn mailbox_of(home: &Path, work: &Path) -> PathBuf {
    home.join("mailbox")
        .join(format!("{}.jsonl", flint::session::dir_key(work)))
}

/// The one session file this home holds. An interactive run prints no machine-readable stream, so the
/// file is found rather than asked for -- and one run writes exactly one.
fn only_session(home: &Path) -> PathBuf {
    let mut found = Vec::new();
    let mut stack = vec![home.join("sessions")];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "jsonl") {
                found.push(path);
            }
        }
    }
    assert_eq!(found.len(), 1, "expected one session file, found {found:?}");
    found.pop().expect("one session")
}

#[tokio::test]
async fn a_peers_words_reach_the_person_and_the_session_but_never_the_model() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let server = MockServer::start().await;
    let (home, work) = scratch("basic", &server.uri());
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(Talking {
            seen: seen.clone(),
            home: home.clone(),
            work: work.clone(),
            said: Mutex::new(false),
        })
        .mount(&server)
        .await;

    let mailbox = mailbox_of(&home, &work);

    // The interactive path, because that is where a run is between turns: a message that arrives while
    // a turn runs is shown when the turn ends, and a one-shot `-p` run never comes back to the prompt.
    // Not `--json`: that flag is for one-shot mode and refuses a prompt-less run, which is honest --
    // the machine-readable stream is for a caller, and a caller has a prompt to give.
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_flint"))
        .args(["--cwd", &work.display().to_string()])
        .env("FLINT_HOME", &home)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to run flint");
    {
        use std::io::Write;
        let stdin = child.stdin.as_mut().expect("stdin");
        stdin.write_all(b"hello there\n").expect("write");
        stdin.flush().expect("flush");
    }
    // Closing stdin is what ends a run at the prompt, and the drain happens before the read that
    // notices the end -- so the message written during the turn is shown either way.
    child.stdin.take();
    let out = child.wait_with_output().expect("flint did not finish");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();

    // 1. The person sees it, and sees that the model did not.
    assert!(
        stdout.contains("says:") && stdout.contains("docs/sandbox.md"),
        "the message never reached the person: {stdout}  stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("not sent to the model"),
        "the transcript does not say that the model has not seen it: {stdout}"
    );

    // 2. The session file keeps it, as its own event and not as a chat message.
    let session = only_session(&home);
    let recorded = std::fs::read_to_string(&session).expect("reading the session");
    let peer_line = recorded
        .lines()
        .find(|line| line.contains("\"type\":\"peer\""))
        .unwrap_or_else(|| panic!("no peer event in {recorded}"));
    assert!(peer_line.contains("docs/sandbox.md"), "{peer_line}");
    assert!(
        !recorded.contains("\\\"role\\\":\\\"user\\\",\\\"content\\\":\\\"please do not"),
        "a peer's words were written as a chat message, which is what a request is built from"
    );

    // 3. The rule, checked against the bytes the provider received: the model was never told.
    let bodies = seen.lock().expect("seen lock").clone();
    assert!(!bodies.is_empty(), "the provider was never asked anything");
    for body in &bodies {
        assert!(
            !body.contains("docs/sandbox.md"),
            "a peer's words reached a request body: {body}"
        );
    }

    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_file(&mailbox);
}

/// The opt-in, which is the whole difference between a mailbox and an injection channel.
///
/// With `--hear-peers`, the message a peer left between two turns is relayed to the model on the next
/// request -- and only there: the session records that it was heard, and the history a *resumed*
/// conversation is rebuilt from still cannot contain it, because the event it was written as is not a
/// chat message. That second half is the property worth a test, not the first: an opt-in that quietly
/// persisted into every later run would be a decision made once and never again.
#[tokio::test]
async fn a_run_that_asked_to_hear_peers_relays_them_and_a_resumed_one_does_not_inherit_them() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let server = MockServer::start().await;
    let (home, work) = scratch("hears", &server.uri());
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(Talking {
            seen: seen.clone(),
            home: home.clone(),
            work: work.clone(),
            said: Mutex::new(false),
        })
        .mount(&server)
        .await;

    let mailbox = mailbox_of(&home, &work);
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_flint"))
        .args(["--hear-peers", "--cwd", &work.display().to_string()])
        .env("FLINT_HOME", &home)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to run flint");

    // Two turns, and the second one has to be asked for at the right moment: the peer speaks during
    // the first turn, the mailbox is read when that turn ends, and a line typed while a turn is
    // running *steers* it instead of becoming the next question. So the transcript is read as it
    // arrives and the second line goes in when the peer's message has been shown -- which is the
    // moment the run is back at its prompt, and is the same fact this test is about.
    let transcript = Arc::new(Mutex::new(String::new()));
    let reader = {
        let sink = transcript.clone();
        let mut out = child.stdout.take().expect("stdout");
        std::thread::spawn(move || {
            use std::io::Read;
            let mut buf = [0u8; 4096];
            while let Ok(n) = out.read(&mut buf) {
                if n == 0 {
                    break;
                }
                sink.lock().expect("transcript lock").push_str(&String::from_utf8_lossy(&buf[..n]));
            }
        })
    };
    {
        use std::io::Write;
        let stdin = child.stdin.as_mut().expect("stdin");
        stdin.write_all(b"hello there\n").expect("write");
        stdin.flush().expect("flush");
    }
    let began = std::time::Instant::now();
    while !transcript.lock().expect("transcript lock").contains("says:") {
        assert!(
            began.elapsed() < std::time::Duration::from_secs(30),
            "the peer's message never reached the person: {}",
            transcript.lock().expect("transcript lock")
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    {
        use std::io::Write;
        let stdin = child.stdin.as_mut().expect("stdin");
        stdin.write_all(b"and now revise the plan\n").expect("write");
        stdin.flush().expect("flush");
    }
    child.stdin.take();
    let status = child.wait().expect("flint did not finish");
    reader.join().expect("the reader thread");
    let stdout = transcript.lock().expect("transcript lock").clone();
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        use std::io::Read;
        let _ = pipe.read_to_string(&mut stderr);
    }
    assert!(status.success(), "the run failed: {stderr}");

    // 1. The person is told the truth about this one: it *was* passed on.
    assert!(
        stdout.contains("docs/sandbox.md"),
        "the message never reached the person: {stdout}  stderr: {stderr}"
    );
    assert!(
        stdout.contains("passed on to the model"),
        "the transcript does not say the model was given it: {stdout}"
    );
    assert!(
        !stdout.contains("not sent to the model"),
        "the transcript still claims the model has not seen it: {stdout}"
    );

    // 2. It reached the request that followed -- and not the one that was in flight, because a mailbox
    //    is read between turns rather than in the middle of a tool loop.
    let bodies = seen.lock().expect("seen lock").clone();
    assert_eq!(bodies.len(), 2, "expected two turns: {bodies:?}");
    assert!(
        !bodies[0].contains("docs/sandbox.md"),
        "the peer's words were in a request that was already being written: {}",
        bodies[0]
    );
    assert!(
        bodies[1].contains("docs/sandbox.md"),
        "--hear-peers relayed nothing: {}",
        bodies[1]
    );
    assert!(
        bodies[1].contains("peer"),
        "the relayed message does not say where it came from: {}",
        bodies[1]
    );

    // 3. The session file says it was heard, and the history a later run is built from does not have it.
    let session = only_session(&home);
    let recorded = std::fs::read_to_string(&session).expect("reading the session");
    let peer_line = recorded
        .lines()
        .find(|line| line.contains("\"type\":\"peer\""))
        .unwrap_or_else(|| panic!("no peer event in {recorded}"));
    assert!(
        peer_line.contains("\"heard\":true"),
        "the record does not say the model was given it: {peer_line}"
    );
    let resumed = flint::session::load(&session).expect("loading the session back");
    for message in &resumed.messages {
        let text = format!("{message:?}");
        assert!(
            !text.contains("docs/sandbox.md"),
            "a peer's words came back as history, which is what the opt-in must not do: {text}"
        );
    }

    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_file(&mailbox);
}

#[tokio::test]
async fn saying_something_needs_no_key_and_says_where_it_went() {
    // An empty home with no provider at all: the message is a file, not a model call.
    let home = std::env::temp_dir().join(format!("flint-say-nokey-{}", std::process::id()));
    let work = home.join("work");
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&work).expect("work dir");

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_flint"))
        .args([
            "say",
            "the tree is yours until 15:00",
            "--cwd",
            &work.display().to_string(),
            "--json",
        ])
        .env("FLINT_HOME", &home)
        .output()
        .expect("flint say");
    assert_eq!(
        out.status.code(),
        Some(0),
        "say failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let line = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(line.lines().next().expect("a line")).expect("json");
    assert_eq!(parsed["text"], "the tree is yours until 15:00");
    let mailbox = PathBuf::from(parsed["mailbox"].as_str().expect("mailbox path"));
    assert!(mailbox.exists(), "the mailbox was not written: {mailbox:?}");
    let written = std::fs::read_to_string(&mailbox).expect("reading the mailbox");
    assert!(written.contains("the tree is yours until 15:00"), "{written}");

    // And a second message appends rather than replaces: the file is a conversation, not a slot. Run
    // without `--json` this time, because the sentence a person reads is the other half of the command.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_flint"))
        .args(["say", "second", "--cwd", &work.display().to_string()])
        .env("FLINT_HOME", &home)
        .output()
        .expect("flint say");
    assert_eq!(out.status.code(), Some(0));
    let human = String::from_utf8_lossy(&out.stdout);
    assert!(
        human.contains("never sent to a model"),
        "the human output does not say what happens to the message: {human}"
    );
    let written = std::fs::read_to_string(&mailbox).expect("reading the mailbox");
    assert_eq!(written.lines().count(), 2, "{written}");

    let _ = std::fs::remove_dir_all(&home);
}
