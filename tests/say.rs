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
        human.contains("shows it to its person"),
        "the human output does not say what happens to the message: {human}"
    );
    // The sentence it used to carry -- "it is never sent to a model" -- was true until `--hear-peers`
    // was built and false afterwards: a listener that asked to hear peers passes what it hears to its
    // own model. The sender cannot know which listeners asked, so the claim has to name the case.
    assert!(
        human.contains("--hear-peers also passes it to its model"),
        "the human output still promises no model will ever see it: {human}"
    );
    let written = std::fs::read_to_string(&mailbox).expect("reading the mailbox");
    assert_eq!(written.lines().count(), 2, "{written}");

    let _ = std::fs::remove_dir_all(&home);
}

/// An interactive run in `work`, with its stdin and stdout piped: the shape both tests below need,
/// because a mailbox is read *between* turns, so a test has to know when a run is back at its prompt
/// rather than only that it exited.
fn interactive(home: &Path, work: &Path) -> std::process::Child {
    std::process::Command::new(env!("CARGO_BIN_EXE_flint"))
        .args(["--cwd", &work.display().to_string()])
        .env("FLINT_HOME", home)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to run flint")
}

/// Type one line at a run.
fn type_line(child: &mut std::process::Child, line: &str) {
    use std::io::Write;
    let stdin = child.stdin.as_mut().expect("stdin");
    stdin.write_all(line.as_bytes()).expect("write");
    stdin.write_all(b"\n").expect("write");
    stdin.flush().expect("flush");
}

/// A run's stdout as it arrives, so a test can wait for a sentence instead of for a process.
fn watch(child: &mut std::process::Child) -> Arc<Mutex<String>> {
    let sink = Arc::new(Mutex::new(String::new()));
    let mut out = child.stdout.take().expect("stdout");
    let theirs = sink.clone();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = [0u8; 4096];
        while let Ok(n) = out.read(&mut buf) {
            if n == 0 {
                break;
            }
            theirs
                .lock()
                .expect("transcript lock")
                .push_str(&String::from_utf8_lossy(&buf[..n]));
        }
    });
    sink
}

/// Wait for a sentence to appear, and say what was on screen if it never does.
fn wait_for(transcript: &Arc<Mutex<String>>, needle: &str, what: &str) -> String {
    let began = std::time::Instant::now();
    loop {
        let seen = transcript.lock().expect("transcript lock").clone();
        if seen.contains(needle) {
            return seen;
        }
        assert!(
            began.elapsed() < std::time::Duration::from_secs(30),
            "{what}: waited for {needle:?}, saw: {seen}"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// `/say` from the prompt: the message reaches the mailbox and a peer, and the run that wrote it does
/// not read its own words back as somebody else's.
///
/// The failure this is written against is the obvious first version: the writer follows the same
/// mailbox it writes to, so the next time round the loop it would show itself "peer pid 12345 says:
/// …". That is not a cosmetic bug -- a person reading it has no way to tell their own words from a
/// peer's, and neither has a run that was asked to hear peers.
#[tokio::test]
async fn a_run_that_says_something_does_not_hear_its_own_words_and_a_peer_does() {
    let server = MockServer::start().await;
    let (home, work) = scratch("slash-say", &server.uri());
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string(prose("ANSWER")))
        .mount(&server)
        .await;

    let mailbox = mailbox_of(&home, &work);
    let message = "please do not commit docs/sandbox.md; I am still writing it";

    // The peer first, and it has to be *between turns* before the message is written: a mailbox is
    // followed from wherever it is when the run starts, so a run that started afterwards would be
    // reading from past the line -- which is the design, and would make this test measure nothing.
    let mut peer = interactive(&home, &work);
    let peer_out = watch(&mut peer);
    type_line(&mut peer, "hello there");
    wait_for(&peer_out, "ANSWER", "the peer never finished its first turn");

    // Now the run that speaks, in the same directory and the same home.
    let mut speaker = interactive(&home, &work);
    let speaker_pid = speaker.id();
    type_line(&mut speaker, &format!("/say {message}"));
    type_line(&mut speaker, "carry on");
    speaker.stdin.take();
    let said = speaker.wait_with_output().expect("the speaker did not finish");
    let speaker_stdout = String::from_utf8_lossy(&said.stdout).to_string();
    let speaker_stderr = String::from_utf8_lossy(&said.stderr).to_string();
    assert!(said.status.success(), "the run failed: {speaker_stderr}");

    // 1. It was written down, by the run that said it, addressed to nobody in particular.
    let written = std::fs::read_to_string(&mailbox)
        .unwrap_or_else(|e| panic!("no mailbox at {}: {e}", mailbox.display()));
    let line = written.lines().next().expect("a mailbox line");
    let parsed: serde_json::Value = serde_json::from_str(line).expect("the line is JSON");
    assert_eq!(parsed["text"], message);
    assert_eq!(parsed["from"], format!("pid {speaker_pid}"));
    assert_eq!(parsed["to"], "", "an unaddressed message is addressed to nobody: {line}");

    // 2. The speaker does not hear itself -- and does name the run that will hear it, which is the
    //    sentence a person needs to know the message is not being shouted into an empty room.
    assert!(
        speaker_stdout.contains(&format!("said: {message}")),
        "the command did not report what it wrote: {speaker_stdout}"
    );
    assert!(
        !speaker_stdout.contains("says:"),
        "the run showed its own message as a peer's: {speaker_stdout}"
    );
    assert!(
        speaker_stdout.contains(&format!("pid {} is working here", peer.id())),
        "the reply does not name the run that will see it: {speaker_stdout}"
    );

    // 3. The peer hears it, on the turn boundary, and is told the model was not given it.
    type_line(&mut peer, "and now revise the plan");
    let peer_seen = wait_for(&peer_out, "says:", "the peer never heard the message");
    assert!(
        peer_seen.contains(message),
        "the peer was told somebody spoke but not what they said: {peer_seen}"
    );
    assert!(
        peer_seen.contains("not sent to the model"),
        "the peer's transcript does not say the model was left out of it: {peer_seen}"
    );
    peer.stdin.take();
    let _ = peer.wait_with_output().expect("the peer did not finish");

    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_file(&mailbox);
}

/// `/say` with nothing to say writes no line, and `--to` picks the run it is for.
///
/// Both halves are about the same trap: a message is prose, so anything that is not prose has to be
/// taken out of it first. `flint say` learned this the expensive way -- `--cwd` left inside the text
/// sent a message to the wrong directory's mailbox -- and a slash command that read `--to 4242` as
/// part of the sentence would be the same bug through a different door.
#[tokio::test]
async fn saying_nothing_leaves_no_line_and_a_pid_can_be_addressed() {
    let (home, work) = scratch("slash-say-flags", "http://127.0.0.1:9/v1");
    let mailbox = mailbox_of(&home, &work);

    let mut run = interactive(&home, &work);
    type_line(&mut run, "/say");
    type_line(&mut run, "/say --to 4242 hello there");
    run.stdin.take();
    let out = run.wait_with_output().expect("the run did not finish");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "the run failed: {}", String::from_utf8_lossy(&out.stderr));

    assert!(
        stdout.contains("nothing to say"),
        "an empty /say was not refused with a sentence: {stdout}"
    );
    let written = std::fs::read_to_string(&mailbox)
        .unwrap_or_else(|e| panic!("no mailbox at {}: {e}", mailbox.display()));
    assert_eq!(
        written.lines().count(),
        1,
        "the empty /say wrote a line anyway: {written}"
    );
    let parsed: serde_json::Value =
        serde_json::from_str(written.lines().next().expect("a line")).expect("json");
    assert_eq!(parsed["text"], "hello there", "the flag was left in the message");
    assert_eq!(parsed["to"], "4242", "{written}");
    assert!(
        stdout.contains("to: 4242"),
        "the reply does not say who it was addressed to: {stdout}"
    );
    assert!(
        stdout.contains("nobody else is working here"),
        "with no peer alive the reply must not imply one heard it: {stdout}"
    );

    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_file(&mailbox);
}

/// A project that keeps a `.flint/` directory has one mailbox for the whole project, and that is the
/// file `flint say` writes -- not a copy beside the home's.
///
/// The home's mailbox is one file per *working directory*; the project's is one file per project,
/// which is what lets two installations hear each other at all, and lets a run in `src/` hear a run at
/// the root. It replaces the home's file rather than being written beside it on purpose: a mailbox is
/// a log of events, and one message in two logs would be shown twice by a run that can see both, with
/// no way to tell a duplicate from somebody repeating themselves.
#[test]
fn a_project_marker_holds_one_mailbox_for_the_whole_project() {
    let (home, work) = scratch("marker-mailbox", "http://127.0.0.1:1/v1");
    std::fs::create_dir_all(work.join(".flint")).expect("the marker");
    // Asked from a *subdirectory*, because that is where the walk matters: the marker is above it, and
    // the two runs are in one project rather than in two unrelated directories.
    let nested = work.join("src/deep");
    std::fs::create_dir_all(&nested).expect("nested directory");

    let project_mailbox = work.join(".flint").join("mailbox.jsonl");
    assert_eq!(
        flint::live::mailbox_path(&nested),
        project_mailbox,
        "the reader resolves a different mailbox than the one this test is about to check"
    );

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_flint"))
        .args([
            "say",
            "the tree is yours",
            "--cwd",
            &nested.display().to_string(),
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
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.lines().next().expect("a line")).expect("json");
    assert_eq!(
        PathBuf::from(parsed["mailbox"].as_str().expect("mailbox path")),
        project_mailbox,
        "say reported a mailbox somewhere else, so the project's is not the one in use"
    );
    let written = std::fs::read_to_string(&project_mailbox)
        .unwrap_or_else(|e| panic!("no project mailbox at {}: {e}", project_mailbox.display()));
    assert!(written.contains("the tree is yours"), "{written}");
    assert!(
        !home.join("mailbox").exists(),
        "the home's mailbox was written as well, so one message now sits in two logs"
    );

    let _ = std::fs::remove_dir_all(&home);
}
