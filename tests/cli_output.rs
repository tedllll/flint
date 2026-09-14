// What the CLI writes when nobody is watching.
//
// `flint` is meant to survive pipelines: `flint --help | less`, `flint exec ... |
// grep`, and the one-shot form used from scripts. That only works if a redirected
// run emits no escape codes, and that guarantee is easy to break in one place
// while every other path stays clean -- the banner once leaked colour, and so did
// the start-up error path, because both bypassed the shared colour decision.
//
// These tests run the real binary and count ESC bytes in its output.
use std::process::Command;

use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A recorded SSE body: each line as its own event, which is what the provider expects.
fn sse(lines: &[&str]) -> String {
    let mut out = String::new();
    for line in lines {
        out.push_str(line);
        out.push_str("\n\n");
    }
    out
}

fn binary() -> Command {
    Command::new(env!("CARGO_BIN_EXE_flint"))
}

/// Run with the given args, returning (exit code, combined stdout + stderr).
fn run(args: &[&str]) -> (i32, Vec<u8>) {
    let out = binary()
        .args(args)
        // The ambient environment must not decide the result.
        .env_remove("NO_COLOR")
        .output()
        .expect("failed to run flint");
    let mut combined = out.stdout;
    combined.extend_from_slice(&out.stderr);
    (out.status.code().unwrap_or(-1), combined)
}

fn escape_count(bytes: &[u8]) -> usize {
    bytes.iter().filter(|b| **b == 0x1b).count()
}

#[test]
fn redirected_help_has_no_escape_codes() {
    let (code, out) = run(&["--help"]);
    assert_eq!(code, 0);
    assert!(
        !out.is_empty(),
        "help printed nothing; the assertion below would pass vacuously"
    );
    assert_eq!(escape_count(&out), 0, "help leaked escape codes into a pipe");
}

#[test]
fn redirected_exec_has_no_escape_codes() {
    let (_, out) = run(&["exec", "echo ESCAPE_PROBE"]);
    assert!(String::from_utf8_lossy(&out).contains("ESCAPE_PROBE"));
    assert_eq!(escape_count(&out), 0, "exec leaked escape codes into a pipe");
}

/// The start-up failure path runs before `real_main`, so it has its own colour
/// decision to get wrong.
#[test]
fn redirected_startup_error_has_no_escape_codes() {
    let (code, out) = run(&["-p", "hi", "--provider", "definitely-not-configured"]);
    assert_eq!(code, 1, "expected a failure for an unknown provider");
    assert!(
        String::from_utf8_lossy(&out).contains("flint: error:"),
        "the error was not reported at all: {:?}",
        String::from_utf8_lossy(&out)
    );
    assert_eq!(
        escape_count(&out),
        0,
        "the error path leaked escape codes into a pipe"
    );
}

/// `--no-color` must be honoured even when stdout *is* a terminal, which is the
/// one case the automatic check cannot cover.
#[test]
fn no_color_is_honoured() {
    let (_, out) = run(&["--help", "--no-color"]);
    assert_eq!(escape_count(&out), 0);
}

/// A one-shot run must ignore its stdin.
///
/// It used to feed it to the turn as "steering": `echo OK | flint -p "say OK"` had its
/// own input read as a person interrupting, so the answer was cancelled before it
/// started and the transcript said `interrupted`. Piping into a one-shot run is
/// ordinary, and it must never change what is asked.
#[tokio::test]
async fn a_piped_one_shot_run_ignores_its_stdin() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse(&[
                    r#"data: {"choices":[{"delta":{"content":"PLAIN ANSWER"}}]}"#,
                    r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
                    "data: [DONE]",
                ])),
        )
        .mount(&server)
        .await;

    // A config of its own, so the test never reads or writes the real one. No credential
    // is needed: the stub server does not check.
    let dir = std::env::temp_dir().join(format!("flint-cli-stdin-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(
        dir.join("config.toml"),
        format!(
            "default_provider = \"stub\"\n\n\
             [[providers]]\n\
             name = \"stub\"\n\
             base_url = \"{}\"\n\
             model = \"stub-model\"\n\
             api_key = \"not-a-real-key\"\n",
            server.uri()
        ),
    )
    .expect("failed to write the test config");

    let mut child = binary()
        .args(["-p", "say something"])
        .env("FLINT_HOME", &dir)
        .env_remove("NO_COLOR")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to run flint");
    {
        use std::io::Write;
        let stdin = child.stdin.as_mut().expect("no stdin handle");
        stdin.write_all(b"steer me\n").expect("failed to write stdin");
    }
    let out = child.wait_with_output().expect("flint did not finish");
    let _ = std::fs::remove_dir_all(&dir);

    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        err.is_empty(),
        "flint wrote to stderr: {err:?} (stdout was {text:?})"
    );
    assert!(
        text.contains("PLAIN ANSWER"),
        "the answer did not arrive: {text:?}"
    );
    assert!(
        !text.contains("interrupted"),
        "stdin was treated as steering: {text:?}"
    );
}

/// Whether a line is one of the marker definitions the scan itself needs.
///
/// Recognised by shape rather than by line number, so adding a marker does not silently
/// re-open the hole: a marker is a character literal in the marker table, or the array's
/// own declaration.
fn is_marker_definition(line: &str) -> bool {
    let t = line.trim();
    // The table itself, its entries, and the prose that explains what the damage looks
    // like -- all of which have to name the characters they are about.
    t.starts_with("const MARKERS")
        || (t.starts_with('\'') && t.contains("\\u{"))
        || t.starts_with("///")
}

/// The source tree must not contain mojibake.
///
/// Not hypothetical: this happened twice, to user-visible text and to a test fixture.
/// A UTF-8 em dash (`e2 80 94`) read as CP936 and written back out became `閳?`, which
/// shipped in fifteen user-visible strings -- including the line a user sees the moment
/// they start a session read-only. Later, `" 鐢ㄦ埛"` in a test became `" 閻劍鍩?`.
///
/// The build notices nothing, because the damage is valid UTF-8 either way: the file
/// compiles and the tests pass, and the corruption is only visible on screen. So this
/// checks the *text*.
///
/// The character set below is how CP936 renders UTF-8 three-byte sequences. Those code
/// points are real Chinese characters, but they are vanishingly rare in ordinary prose,
/// so finding one inside a string or comment means something was mis-decoded. Matching
/// on a fixed list of complete corrupted strings is not enough -- that was the first
/// version of this test, and it missed `閻劍鍩沗 entirely.
#[test]
fn the_source_tree_contains_no_mojibake() {
    // Characters CP936 produces when it swallows a UTF-8 multi-byte sequence. Any of
    // these in this repository is an artifact, not prose.
    const MARKERS: &[char] = &[
        '\u{9225}', // 閳? -- half of the em dash above
        '\u{9429}', '\u{951b}', '\u{9422}', // 閿?閿?閻?        '\u{3126}', '\u{57db}', // 銊?鍩?-- pieces of 鐢ㄦ埛
        '\u{8def}', // 璺?-- a middle dot (U+00B7) mis-read as CP936. Ordinary-looking
                     // Chinese, which is exactly why it survived a scan for obvious
                     // garbage; it is listed because this repository has no prose that
                     // would use it. 璐?澶?椤?are NOT listed for that reason: "椤瑰け璐?
                     // is ordinary Chinese in the layout script's own output.
        '\u{9428}', '\u{93b4}', '\u{93c1}', '\u{93c8}', '\u{93c5}',
        '\u{fffd}', // the replacement character: data already lost
    ];

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut offenders: Vec<String> = Vec::new();
    let mut scanned = 0usize;

    let mut walk = vec![root.to_path_buf()];
    while let Some(dir) = walk.pop() {
        for entry in std::fs::read_dir(&dir).expect("read_dir") {
            let path = entry.expect("dir entry").path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name == ".git" || name == "target" || name == "node_modules" {
                continue;
            }
            if path.is_dir() {
                walk.push(path);
                continue;
            }
            let is_text = matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("rs" | "js" | "md" | "toml" | "yml" | "yaml")
            );
            if !is_text {
                continue;
            }
            scanned += 1;
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            for (n, line) in text.lines().enumerate() {
                if !MARKERS.iter().any(|m| line.contains(*m)) {
                    continue;
                }
                // This file has to name the characters it looks for, and the marker
                // definitions are the only lines where that is legitimate. Excluding the
                // whole file instead -- which this used to do -- leaves the one file that
                // is *about* encoding damage as the one file damage could hide in.
                if path.ends_with("cli_output.rs") && is_marker_definition(line) {
                    continue;
                }
                offenders.push(format!(
                    "{}:{}: {}",
                    path.strip_prefix(root).unwrap_or(&path).display(),
                    n + 1,
                    line.trim()
                ));
            }
        }
    }

    assert!(scanned > 5, "the walk found almost nothing ({scanned} files)");
    assert!(
        offenders.is_empty(),
        "mojibake in the source tree -- a UTF-8 file was written back through a CP936 \
         code page:\n{}",
        offenders.join("\n")
    );
}

/// `exec` must run a command without a config, and must not create one.
///
/// This is the whole point of `exec`: when every provider is unreachable it still has to
/// work, which is exactly the situation where no config exists yet. The old path called
/// the config loader that creates a default file, so `exec echo hi` printed
///
///   flint: created default config at .../config.toml
///   flint: set your API key there (or export DEEPSEEK_API_KEY), then re-run.
///
/// and then ran the command anyway. Two things wrong with that: it writes to the user's
/// home directory as a side effect of asking for one echo, and it tells them to go set an
/// API key that `exec` never uses -- the one message guaranteed to stop someone mid-rescue
/// and send them debugging the wrong thing.
#[test]
fn exec_works_without_a_config_and_creates_none() {
    let home = std::env::temp_dir().join(format!("flint-exec-noconfig-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).expect("temp home");

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_flint"))
        .arg("exec")
        .arg("echo exec-works")
        // Both variables, because the config directory is resolved per platform.
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        // A key in the ambient environment would mask nothing here, but leaving it set
        // would make the test's own intent unclear.
        .env_remove("DEEPSEEK_API_KEY")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run flint exec");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(out.status.success(), "exec failed: {stderr}");
    assert!(
        stdout.contains("exec-works"),
        "the command did not run: stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        !stderr.contains("API key"),
        "exec must not ask for an API key it never uses: {stderr:?}"
    );
    assert!(
        !stderr.contains("created default config"),
        "exec must not announce config creation: {stderr:?}"
    );
    assert!(
        !home.join(".flint").exists(),
        "exec created a config directory as a side effect: {}",
        home.display()
    );

    let _ = std::fs::remove_dir_all(&home);
}

/// A `FLINT_HOME` of this test's own: a config, and an empty sessions directory.
fn test_home(tag: &str, base_url: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("flint-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sessions")).expect("temp home");
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
    .expect("failed to write the test config");
    dir
}

/// Write a session file, ordering it by modification time.
///
/// The order matters because the list is newest-first and the numbers are what
/// `--archive N` takes, so a test that shrugged at it would be asserting against
/// whatever order the filesystem happened to produce.
fn write_session(
    dir: &std::path::Path,
    file: &str,
    lines: &[&str],
    age_secs: u64,
) -> std::path::PathBuf {
    let path = dir.join(file);
    std::fs::write(&path, format!("{}\n", lines.join("\n"))).expect("write session");
    let when = std::time::SystemTime::now() - std::time::Duration::from_secs(age_secs);
    let handle = std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .expect("open session");
    handle.set_modified(when).expect("set mtime");
    path
}

fn meta_line(id: &str) -> String {
    format!(
        r#"{{"type":"meta","v":2,"id":"{id}","created":"epoch:1","cwd":"/tmp","provider":"stub","model":"stub-model"}}"#
    )
}

/// Tidying sessions is a file operation, and it must not need a model.
///
/// `--list-sessions`, `--archive` and `--delete` all run before the provider is even
/// resolved, because the moment you want to tidy the list is often the moment the
/// network is what is broken. This drives the real binary with no server at all.
#[test]
fn sessions_can_be_listed_named_archived_and_deleted_without_a_model() {
    let home = test_home("sessions", "http://127.0.0.1:1/v1");
    let sessions = home.join("sessions");

    // Newest first, and each one exercises a different part of the format: a name
    // appended after the conversation, an event this build has never heard of, and one
    // already filed away.
    write_session(
        &sessions,
        "111-1.jsonl",
        &[
            &meta_line("111-1"),
            r#"{"type":"chat","message":{"role":"user","content":"the codex config is broken"}}"#,
            r#"{"type":"title","name":"codex config"}"#,
        ],
        10,
    );
    write_session(
        &sessions,
        "222-2.jsonl",
        &[
            &meta_line("222-2"),
            r#"{"type":"chat","message":{"role":"user","content":"update dsh please"}}"#,
            r#"{"type":"future-event","payload":{"unknown":true}}"#,
        ],
        20,
    );
    std::fs::create_dir_all(sessions.join("archive")).expect("archive dir");
    write_session(
        &sessions.join("archive"),
        "333-3.jsonl",
        &[
            &meta_line("333-3"),
            r#"{"type":"chat","message":{"role":"user","content":"long forgotten"}}"#,
        ],
        30,
    );

    let out = binary()
        .arg("--list-sessions")
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .output()
        .expect("failed to run flint");
    let listed = String::from_utf8_lossy(&out.stdout).to_string();
    let warned = String::from_utf8_lossy(&out.stderr).to_string();

    assert!(out.status.success(), "listing failed: {warned}");
    assert!(
        listed.contains("codex config"),
        "the name given to a session is not shown: {listed:?}"
    );
    assert!(
        listed.contains("update dsh please"),
        "an unnamed session lost its first message: {listed:?}"
    );
    assert!(
        !listed.contains("long forgotten"),
        "an archived session is still being listed: {listed:?}"
    );
    // The unknown event must be ignored in silence. A future version's session is not
    // corruption, and saying so on every list would make it look like it.
    assert!(
        warned.is_empty(),
        "an unknown event was reported as damage: {warned:?}"
    );

    // `--archive 1` is the newest, which the title above identifies.
    let out = binary()
        .args(["--archive", "1"])
        .env("FLINT_HOME", &home)
        .output()
        .expect("failed to run flint");
    assert!(
        out.status.success(),
        "archiving failed: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        sessions.join("archive").join("111-1.jsonl").exists(),
        "the archived session is not in the archive directory"
    );
    assert!(!sessions.join("111-1.jsonl").exists());

    // By id prefix, so a session can be removed without knowing its number.
    let out = binary()
        .args(["--delete", "222-2"])
        .env("FLINT_HOME", &home)
        .output()
        .expect("failed to run flint");
    assert!(
        out.status.success(),
        "deleting failed: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!sessions.join("222-2.jsonl").exists());

    // Archived sessions are out of the list but not out of reach: resolving by id has to
    // keep working, or `mv` by hand would make a conversation unreachable.
    let out = binary()
        .args(["--archive", "333-3"])
        .env("FLINT_HOME", &home)
        .output()
        .expect("failed to run flint");
    assert!(
        out.status.success(),
        "an archived session could not be resolved by id: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        sessions.join("archive").join("333-3.jsonl").exists(),
        "archiving an already-archived session moved it somewhere else"
    );

    let _ = std::fs::remove_dir_all(&home);
}

/// Naming appends, so it works on a conversation that already happened.
///
/// The shape of this is the point: `--name` on a resumed session must not rewrite
/// anything, and the *last* name in the file is the one that counts.
#[cfg(debug_assertions)]
#[tokio::test]
async fn a_session_can_be_named_after_the_fact_and_the_last_name_wins() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse(&[
                    r#"data: {"choices":[{"delta":{"content":"ok"}}]}"#,
                    r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
                    "data: [DONE]",
                ])),
        )
        .mount(&server)
        .await;

    let home = test_home("naming", &server.uri());

    let first = binary()
        .args(["-p", "say something", "--name", "first name"])
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("failed to run flint");
    assert!(
        first.status.success(),
        "the named run failed: {:?}",
        String::from_utf8_lossy(&first.stderr)
    );

    // The same conversation, continued and renamed.
    let resume_args = std::fs::read_dir(home.join("sessions"))
        .expect("sessions dir")
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().and_then(|e| e.to_str()) == Some("jsonl"))
        .expect("the run did not write a session");
    assert!(
        std::fs::read_to_string(&resume_args)
            .expect("read session")
            .contains(r#""type":"title","name":"first name""#),
        "the name was not appended to the session file"
    );

    let second = binary()
        .args(["-p", "and again", "--continue", "--name", "second name"])
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("failed to run flint");
    assert!(
        second.status.success(),
        "resuming with a new name failed: {:?}",
        String::from_utf8_lossy(&second.stderr)
    );

    let listed = binary()
        .arg("--list-sessions")
        .env("FLINT_HOME", &home)
        .output()
        .expect("failed to run flint");
    let listed = String::from_utf8_lossy(&listed.stdout).to_string();
    assert!(
        listed.contains("second name"),
        "the newest name is not the one shown: {listed:?}"
    );
    assert!(
        !listed.contains("first name"),
        "an older name is still in force: {listed:?}"
    );

    let _ = std::fs::remove_dir_all(&home);
}

/// The REPL's own session commands, driven through a real session.
///
/// `/name` writes to the conversation that is open, and `/archive` and `/delete` refuse
/// it -- a rule worth a test, because the failure it prevents is silent: deleting the
/// file this process appends to would have it recreated by the next event, and the
/// conversation would come back as a nameless fragment.
#[cfg(debug_assertions)]
#[tokio::test]
async fn the_repl_names_the_open_session_and_refuses_to_delete_it() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse(&[
                    r#"data: {"choices":[{"delta":{"content":"ok"}}]}"#,
                    r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
                    "data: [DONE]",
                ])),
        )
        .mount(&server)
        .await;

    let home = test_home("repl-name", &server.uri());
    let mut child = binary()
        .env("FLINT_HOME", &home)
        .env("FLINT_TERM_CAPTURE", "1")
        .env("FLINT_TERM_SIZE", "80x24")
        .env_remove("NO_COLOR")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to run flint");
    {
        use std::io::Write;
        let stdin = child.stdin.as_mut().expect("no stdin handle");
        stdin
            .write_all(b"/name a named conversation\n/sessions\n/delete 1\n/exit\n")
            .expect("failed to write stdin");
    }
    let out = child.wait_with_output().expect("flint did not finish");
    let text = String::from_utf8_lossy(&out.stdout).to_string();

    let session = std::fs::read_dir(home.join("sessions"))
        .expect("sessions dir")
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().and_then(|e| e.to_str()) == Some("jsonl"))
        .expect("no session file was written");
    let body = std::fs::read_to_string(&session).expect("read session");

    assert!(
        body.contains(r#""type":"title","name":"a named conversation""#),
        "the name never reached the session file: {body:?}"
    );
    assert!(
        text.contains("a named conversation"),
        "the listing does not show the name: {text:?}"
    );
    assert!(
        text.contains("that is the conversation you are in"),
        "deleting the open session was not refused: {text:?}"
    );
    assert!(
        session.exists(),
        "the open session was deleted anyway: {}",
        session.display()
    );

    let _ = std::fs::remove_dir_all(&home);
}

/// `/skills` has to show what the model would actually be handed.
///
/// The REPL is the only place a person can check that, and the check that matters is the
/// body: a catalog naming a skill whose file cannot be read is worse than no catalog,
/// because the model is then told to load something that is not there.
#[cfg(debug_assertions)]
#[test]
fn the_repl_lists_skills_and_prints_one_the_way_the_model_gets_it() {
    let home = test_home("repl-skills", "http://127.0.0.1:1/v1");
    let work = home.join("work");
    std::fs::create_dir_all(work.join(".git")).expect("project dir");
    let skill_dir = work.join(".flint").join("skills").join("tidy-commits");
    std::fs::create_dir_all(&skill_dir).expect("skills dir");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: tidy-commits\ndescription: Squash and reword the commits.\n---\n\nStep one: squash the fixups.\n",
    )
    .expect("skill file");

    let mut child = binary()
        .current_dir(&work)
        .env("FLINT_HOME", &home)
        .env("FLINT_TERM_CAPTURE", "1")
        .env("FLINT_TERM_SIZE", "100x24")
        .env_remove("NO_COLOR")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to run flint");
    {
        use std::io::Write;
        let stdin = child.stdin.as_mut().expect("no stdin handle");
        stdin
            .write_all(b"/skills\n/skills tidy-commits\n/exit\n")
            .expect("failed to write stdin");
    }
    let out = child.wait_with_output().expect("flint did not finish");
    let text = String::from_utf8_lossy(&out.stdout).to_string();

    assert!(
        text.contains("tidy-commits"),
        "the skill is not listed: {text:?}"
    );
    assert!(
        text.contains("Squash and reword the commits."),
        "the summary is not shown: {text:?}"
    );
    assert!(
        text.contains("SKILL.md"),
        "the listing does not say where the skill came from: {text:?}"
    );
    assert!(
        text.contains("Step one: squash the fixups."),
        "`/skills <name>` did not print the body: {text:?}"
    );

    let _ = std::fs::remove_dir_all(&home);
}

/// The clock has to be running for the wait *after* a tool round, and not for the tools.
/// Two things were wrong with it. A tool fast enough that the hold-back never expired
/// still painted `鈹€鈹€ 0s <tool> 鈹€鈹€`, because committing a transcript line repainted the
/// status row without waiting; and the first tool result stopped the clock outright, so a
/// round of several calls ran mostly untimed and the model call that followed it showed
/// no clock at all -- the pause a user is actually staring at.
///
/// Driven through the real binary because this is the wiring, not the drawing: the events
/// that start and stop the clock come from the agent loop, and no test of `Term` alone
/// can see which of them arrive when.
#[cfg(debug_assertions)]
#[tokio::test]
async fn a_tool_round_leaves_the_clock_running_for_the_model_call_after_it() {
    let server = MockServer::start().await;

    // The round: one tool call. `echo` is the one command both `cmd /C` and `sh -c`
    // understand, which keeps the test from depending on the platform's shell.
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse(&[
                    r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_0","function":{"name":"bash","arguments":"{\"command\":\"echo hi\"}"}}]}}]}"#,
                    r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
                    "data: [DONE]",
                ])),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;

    // The model call after the tool, slow enough for the clock to tick.
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_delay(std::time::Duration::from_millis(2000))
                .set_body_string(sse(&[
                    r#"data: {"choices":[{"delta":{"content":"done"}}]}"#,
                    r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
                    "data: [DONE]",
                ])),
        )
        .mount(&server)
        .await;

    let home = std::env::temp_dir().join(format!("flint-clock-{}", std::process::id()));
    let work = home.join("work");
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&work).expect("temp dirs");
    std::fs::write(
        home.join("config.toml"),
        format!(
            "default_provider = \"stub\"\n\n\
             [[providers]]\n\
             name = \"stub\"\n\
             base_url = \"{}\"\n\
             model = \"stub-model\"\n\
             api_key = \"not-a-real-key\"\n",
            server.uri()
        ),
    )
    .expect("failed to write the test config");

    let out = binary()
        .args(["-p", "run it", "--cwd"])
        .arg(&work)
        .env("FLINT_HOME", &home)
        // The interactive layout: the status row only exists in that one, and the size
        // decides where every row lands in the bytes asserted on below.
        .env("FLINT_TERM_CAPTURE", "1")
        .env("FLINT_TERM_SIZE", "80x24")
        .env_remove("NO_COLOR")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("failed to run flint");
    let _ = std::fs::remove_dir_all(&home);

    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        text.contains("waiting for the model \u{2500}\u{2500}"),
        "the model call after the tool round has no clock:\n{text:?}"
    );
    assert!(
        text.contains("1s waiting for the model \u{2500}\u{2500}"),
        "the clock stopped instead of counting while the model was waited for:\n{text:?}"
    );
    assert!(
        !text.contains("bash \u{2500}\u{2500}"),
        "a tool that finished inside the hold-back still got a clock:\n{text:?}"
    );
}

/// `flint debug prompt-input` prints the request it would send, and sends nothing.
///
/// The second half is the one worth asserting. A diagnostic that quietly created a session
/// file would leave a conversation behind for a run that never happened -- it would appear
/// in `/sessions`, and resuming it would open a transcript of nothing. So the check is not
/// only that the JSON is right, but that the home directory it was pointed at is otherwise
/// untouched.
#[test]
fn debug_prompt_input_prints_the_request_body_and_creates_no_session() {
    let home = std::env::temp_dir().join(format!("flint-debug-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).expect("home directory");
    // An endpoint that cannot be reached, on purpose: nothing in this command may talk to
    // it, and a URL that answers would hide a request that should not have been made.
    std::fs::write(
        home.join("config.toml"),
        "default_provider = \"stub\"\n\n\
         [[providers]]\n\
         name = \"stub\"\n\
         base_url = \"http://127.0.0.1:1/v1\"\n\
         api_key = \"test\"\n\
         model = \"stub-model\"\n",
    )
    .expect("config file");

    let out = binary()
        .args(["debug", "prompt-input", "why is the build failing"])
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .output()
        .expect("failed to run flint");

    assert_eq!(
        out.status.code(),
        Some(0),
        "debug failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let body: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "the output is not one JSON document ({e}): {:?}",
            String::from_utf8_lossy(&out.stdout)
        )
    });

    let messages = body["messages"].as_array().expect("messages");
    assert_eq!(messages[0]["role"], "system");
    assert!(
        messages[0]["content"]
            .as_str()
            .unwrap_or_default()
            .contains("You are flint"),
        "the system prompt is the first thing the model reads"
    );
    assert_eq!(
        messages.last().expect("a last message")["content"],
        "why is the build failing",
        "the message after the subcommand is the one that would be sent"
    );
    assert_eq!(body["model"], "stub-model");
    assert_eq!(body["stream"], true);
    assert!(
        !body["tools"].as_array().expect("tools").is_empty(),
        "the tool schemas are part of what is sent"
    );

    assert_eq!(escape_count(&out.stdout), 0, "the JSON leaked escape codes");
    assert!(
        !home.join("sessions").exists(),
        "a diagnostic that sends nothing created a session file"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// The `debug` namespace says what it knows rather than failing silently.
#[test]
fn an_unknown_debug_subcommand_names_the_ones_that_exist() {
    let (code, out) = run(&["debug", "nonsense"]);
    let text = String::from_utf8_lossy(&out);
    assert_ne!(code, 0, "an unknown subcommand must not look like success");
    assert!(
        text.contains("prompt-input"),
        "the error must name what exists: {text}"
    );

    let (code, out) = run(&["debug"]);
    assert_ne!(code, 0);
    assert!(
        String::from_utf8_lossy(&out).contains("prompt-input"),
        "a bare `debug` must name its subcommands too"
    );
}

/// Mount a stub provider that answers once, for a REPL run that makes one request.
async fn answer_once(server: &MockServer) {
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse(&[
                    r#"data: {"choices":[{"delta":{"content":"STUB ANSWER"}}]}"#,
                    r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
                    "data: [DONE]",
                ])),
        )
        .mount(server)
        .await;
}

/// Resume a one-session home in the REPL and return (stdout, the first request body).
///
/// The stub stands in for the provider so the test can read what was actually sent: the
/// presence or absence of a system prompt is invisible anywhere else in the run.
async fn resume_and_capture(
    server: &MockServer,
    tag: &str,
    fixture: &[&str],
) -> (String, serde_json::Value) {
    // A home of its own per call: `test_home` keys on the process id, and two tests in
    // this binary run at the same time, so a shared name means one deletes the other's
    // fixture mid-run and the failure reads as a missing request.
    let home = test_home(tag, &server.uri());
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("working directory");
    write_session(&home.join("sessions"), "111-1.jsonl", fixture, 10);

    let mut child = binary()
        .current_dir(&work)
        .env("FLINT_HOME", &home)
        .env("FLINT_TERM_CAPTURE", "1")
        .env("FLINT_TERM_SIZE", "100x24")
        .env_remove("NO_COLOR")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to run flint");
    {
        use std::io::Write;
        let stdin = child.stdin.as_mut().expect("no stdin handle");
        // By id, not by list number: the REPL creates its own session on startup, and that
        // one is the newest, so `1` would resume the empty file this run just opened.
        // No `/exit`: with piped stdin it would arrive while `hello` is still being
        // answered, and it is now run as the command it is -- which cancels the turn
        // before any request goes out. Closing the pipe ends the REPL instead, after
        // the turn has finished, which is what this test needs to observe.
        stdin
            .write_all(b"/resume 111-1\nhello\n")
            .expect("failed to write stdin");
    }
    let out = child.wait_with_output().expect("flint did not finish");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();

    // Read the bodies the stub was sent, after the child has finished: `Respond` needs an
    // `Fn`, so the recording cannot happen inside the responder.
    let requests = server.received_requests().await.expect("requests");
    let body = requests
        .first()
        .map(|r| serde_json::from_slice(&r.body).expect("the request body is JSON"))
        .expect("no request was made after /resume, so no prompt was ever sent");

    let _ = std::fs::remove_dir_all(&home);
    (stdout, body)
}

/// Resuming an ordinary session must send the model a system prompt.
///
/// A session file holds the conversation and **not** the prompt: the prompt is rebuilt at
/// startup on purpose, because it carries run-time facts -- the shell dialect, the working
/// directory, the instruction files -- that a transcript cannot be trusted to still be
/// right about. `/resume` replaced the whole history with the loaded one, and since an
/// ordinary file has no system message in it, that dropped the freshly built prompt and
/// left the model with no instructions at all: no tool guidance, no "act, do not narrate",
/// no note about where commands run.
///
/// Nothing in the run makes that visible. The REPL looks perfectly normal, the answers just
/// quietly get worse, and the only place the loss shows up is the request itself -- which is
/// what this reads.
#[tokio::test]
async fn resuming_a_session_keeps_a_system_prompt() {
    let server = MockServer::start().await;
    answer_once(&server).await;
    let (_stdout, sent) = resume_and_capture(
        &server,
        "repl-resume-keeps",
        &[
            &meta_line("111-1"),
            r#"{"type":"chat","message":{"role":"user","content":"the earlier question"}}"#,
            r#"{"type":"chat","message":{"role":"assistant","content":"the earlier answer"}}"#,
        ],
    )
    .await;

    let messages = sent["messages"].as_array().expect("messages");
    let roles: Vec<&str> = messages
        .iter()
        .map(|m| m["role"].as_str().unwrap_or("?"))
        .collect();
    assert_eq!(
        roles.first().copied(),
        Some("system"),
        "the resumed conversation was sent with no system prompt at all: {roles:?}"
    );
    assert!(
        messages[0]["content"]
            .as_str()
            .unwrap_or_default()
            .contains("You are flint"),
        "the first message is not flint's instructions: {}",
        messages[0]
    );
    // The conversation itself must survive, which is the point of resuming at all.
    assert!(
        sent.to_string().contains("the earlier question"),
        "the loaded conversation was dropped: {sent}"
    );
}

/// A stored system prompt is replaced, not added to.
///
/// A hand-edited file, or one written by another build from another directory, can carry a
/// system message of its own. Sending it beside the fresh one is worse than sending neither:
/// the model is handed two sets of instructions and believes the one that is wrong.
#[tokio::test]
async fn resuming_does_not_send_a_stale_system_prompt() {
    let server = MockServer::start().await;
    answer_once(&server).await;
    let (_stdout, sent) = resume_and_capture(
        &server,
        "repl-resume-stale",
        &[
            &meta_line("111-1"),
            r#"{"type":"chat","message":{"role":"system","content":"STALE-PROMPT-MARKER working directory /nowhere-at-all"}}"#,
            r#"{"type":"chat","message":{"role":"user","content":"the earlier question"}}"#,
        ],
    )
    .await;

    let messages = sent["messages"].as_array().expect("messages");
    let systems = messages
        .iter()
        .filter(|m| m["role"] == "system")
        .count();
    assert_eq!(systems, 1, "expected exactly one system prompt: {sent}");
    assert!(
        !sent.to_string().contains("STALE-PROMPT-MARKER"),
        "the stored system prompt reached the model: {sent}"
    );
    assert!(
        sent.to_string().contains("the earlier question"),
        "the loaded conversation was dropped: {sent}"
    );
}

/// `--web` says where it is serving, and `--port` without it is refused.
///
/// The socket tests prove the listener answers correctly; nothing in them can see whether the
/// CLI ever bound it or ever told anyone where. A flag that is parsed and dropped is the
/// failure this catches, and it is the one that looks most like success.
#[cfg(debug_assertions)]
#[test]
fn the_web_flag_prints_the_url_it_is_serving() {
    let home = test_home("cli-web", "http://127.0.0.1:1/v1");
    let mut child = binary()
        .args(["--web"])
        .env("FLINT_HOME", &home)
        .env("FLINT_TERM_CAPTURE", "1")
        .env("FLINT_TERM_SIZE", "120x24")
        .env_remove("NO_COLOR")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to run flint");
    {
        use std::io::Write;
        let stdin = child.stdin.as_mut().expect("no stdin handle");
        stdin
            .write_all(b"hello\n")
            .expect("failed to write stdin");
    }
    let out = child.wait_with_output().expect("flint did not finish");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let _ = std::fs::remove_dir_all(&home);

    let start = text
        .find("http://127.0.0.1:")
        .unwrap_or_else(|| panic!("--web printed no URL at all: {text:?}"));
    let url = &text[start..];
    let (authority, rest) = url.split_once("/?token=").unwrap_or_else(|| {
        panic!(
            "the URL must carry the token in a query string, got {:?}",
            &url[..url.len().min(80)]
        )
    });
    let port: u16 = authority
        .trim_start_matches("http://127.0.0.1:")
        .parse()
        .unwrap_or_else(|e| panic!("the URL must name a port: {authority:?} ({e})"));
    assert!(port > 0, "a bound port is never 0, whatever was asked for");

    // The token is the credential, so its shape is worth asserting: 128 bits of hex.
    let token: String = rest.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
    assert_eq!(
        token.len(),
        32,
        "expected a 32-hex-digit token, got {token:?} in {:?}",
        &url[..url.len().min(80)]
    );
}

/// `--port` on its own is a mistake worth naming rather than ignoring.
#[test]
fn a_port_without_web_is_refused() {
    let (code, out) = run(&["--port", "8080"]);
    let text = String::from_utf8_lossy(&out);
    assert_ne!(code, 0, "`--port` alone must not look like success: {text}");
    assert!(
        text.contains("--port needs --web"),
        "the error must say what is missing: {text}"
    );
}

/// Switching provider must actually run the engine's start command.
///
/// This is the test for a bug that shipped and was found by hand: `/provider <name>` built
/// its agent inline instead of going through `switch_provider`, so the engine handling 鈥?/// which lives there 鈥?was skipped on the one path everybody uses. `/provider key` and
/// `/provider rm` did start engines, which is exactly the kind of inconsistency a second
/// copy of four lines produces.
///
/// The start command leaves a file, and the endpoint is a port nothing is listening on, so
/// the wait ends in a second rather than in a model load.
///
/// Two things here have to be written for both platforms, and both were wrong the first time
/// this ran on Windows: the start command goes through the platform's shell, so it is spelled
/// in the one language `cmd /C` and `sh -c` agree on (`echo`), and the marker path is a TOML
/// **literal** string. In a basic string a Windows path is a parse error rather than a path --
/// `C:\Users` is read as a unicode escape, and flint refuses the whole config with "too few
/// unicode value digits". That is TOML being TOML, not a flint bug, but a test that writes a
/// path by `display()` has to know it.
#[cfg(debug_assertions)]
#[test]
fn switching_provider_runs_the_engines_start_command() {
    let home = test_home("engine-switch", "http://127.0.0.1:1/v1");
    let marker = home.join("the-engine-was-started");
    let (shell, shell_args) = if cfg!(windows) {
        ("cmd", "[\"/C\"]")
    } else {
        ("sh", "[\"-c\"]")
    };
    std::fs::write(
        home.join("config.toml"),
        format!(
            "default_provider = \"stub\"\n\
             shell = \"{shell}\"\n\
             shell_args = {shell_args}\n\
             \n\
             [[providers]]\n\
             name = \"stub\"\n\
             base_url = \"http://127.0.0.1:1/v1\"\n\
             api_key = \"x\"\n\
             model = \"stub\"\n\
             \n\
             [[providers]]\n\
             name = \"engine\"\n\
             base_url = \"http://127.0.0.1:9/v1\"\n\
             api_key = \"x\"\n\
             model = \"something\"\n\
             start = 'echo started > {marker}'\n\
             start_timeout_secs = 1\n",
            marker = marker.display()
        ),
    )
    .expect("config");

    let mut child = binary()
        .env("FLINT_HOME", &home)
        .env("FLINT_TERM_CAPTURE", "1")
        .env("FLINT_TERM_SIZE", "120x24")
        .env_remove("NO_COLOR")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to run flint");
    {
        use std::io::Write;
        let stdin = child.stdin.as_mut().expect("no stdin handle");
        stdin
            .write_all(b"/provider engine\n/exit\n")
            .expect("failed to write stdin");
    }
    let out = child.wait_with_output().expect("flint did not finish");
    let text = String::from_utf8_lossy(&out.stdout).to_string();

    assert!(
        text.contains("switched to engine"),
        "the switch itself did not happen, so this proves nothing: {text:?}"
    );
    assert!(
        marker.exists(),
        "the start command never ran on `/provider <name>`: {text:?}"
    );

    let _ = std::fs::remove_dir_all(&home);
}

/// Run the interactive REPL with these lines on stdin and return all of its output.
///
/// Piped stdin is a first-class way to drive flint -- `/exit` ends it -- and it exercises the
/// real command dispatch rather than a copy of it. A pty is needed only for the paths that
/// read *keys*, which is why the paste fix is not covered here (see `HANDOFF.md`).
fn repl(home: &std::path::Path, lines: &[&str]) -> String {
    use std::io::Write;
    let mut child = binary()
        .env("FLINT_HOME", home)
        .env_remove("NO_COLOR")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to run flint");
    {
        let stdin = child.stdin.as_mut().expect("no stdin handle");
        stdin
            .write_all(format!("{}\n", lines.join("\n")).as_bytes())
            .expect("failed to write stdin");
    }
    let out = child.wait_with_output().expect("flint did not finish");
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// A stub that answers every request with a marker, so a test can tell whether the model was
/// reached at all.
async fn marker_provider(marker: &'static str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse(&[
                    &format!(r#"data: {{"choices":[{{"delta":{{"content":"{marker}"}}}}]}}"#),
                    r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
                    "data: [DONE]",
                ])),
        )
        .mount(&server)
        .await;
    server
}

/// `--web` typed at the prompt opens the view, and never reaches the model.
///
/// Reported from a real session in exactly this shape, and reported twice, because the first
/// fix answered the wrong half: `--web` was entered at the prompt -- it is the only name for
/// the feature a person has met, since it is in `--help`, in the README and in flint's own
/// error messages -- and nothing marked it as belonging to the command line rather than to the
/// conversation. What the person wanted was the page. So the flag is *translated*, not refused:
/// refusing it with an explanation was the first version and was the wrong answer, because they
/// had already said what they wanted and being told to respell it is not help.
#[tokio::test]
async fn a_flag_typed_at_the_prompt_does_what_it_names() {
    let server = marker_provider("THE MODEL WAS REACHED").await;
    let home = test_home("prompt-flag", &server.uri());

    let text = repl(&home, &["--web", "/exit"]);
    let _ = std::fs::remove_dir_all(&home);

    assert!(
        text.contains("web: http://127.0.0.1:"),
        "`--web` at the prompt did not open the view: {text:?}"
    );
    assert!(
        !text.contains("THE MODEL WAS REACHED"),
        "`--web` was sent to the model after all: {text:?}"
    );
    // A pipe is not a person, so nothing may be launched. This is the assertion that keeps
    // `cargo test` from opening a browser window on whoever runs it.
    assert!(
        !text.contains("(opening it)"),
        "a browser was launched with stdout redirected: {text:?}"
    );
}

/// A flag with a slash equivalent is turned into that command.
#[tokio::test]
async fn a_flag_at_the_prompt_becomes_the_command_it_names() {
    let server = marker_provider("THE MODEL WAS REACHED").await;
    let home = test_home("flag-to-command", &server.uri());

    let text = repl(&home, &["--help", "/exit"]);
    let _ = std::fs::remove_dir_all(&home);

    assert!(
        text.contains("/sessions"),
        "`--help` did not become `/help`: {text:?}"
    );
    assert!(
        !text.contains("THE MODEL WAS REACHED"),
        "`--help` was sent to the model: {text:?}"
    );
}

/// A flag that only exists at start-up says so, rather than being sent or half-applied.
#[tokio::test]
async fn a_start_up_only_flag_says_so() {
    let server = marker_provider("THE MODEL WAS REACHED").await;
    let home = test_home("flag-startup-only", &server.uri());

    let text = repl(&home, &["--json", "/exit"]);
    let _ = std::fs::remove_dir_all(&home);

    assert!(
        text.contains("only read when flint starts"),
        "the flag was not explained: {text:?}"
    );
    assert!(
        !text.contains("THE MODEL WAS REACHED"),
        "`--json` was sent to the model: {text:?}"
    );
}

/// `/web` opens the browser view of the conversation that is already running.
#[tokio::test]
async fn the_web_command_opens_the_browser_view() {
    let server = marker_provider("THE MODEL WAS REACHED").await;
    let home = test_home("web-command", &server.uri());

    let text = repl(&home, &["/web", "/exit"]);
    let _ = std::fs::remove_dir_all(&home);

    assert!(
        text.contains("web: http://127.0.0.1:"),
        "`/web` did not print a URL: {text:?}"
    );
    assert!(
        !text.contains("(opening it)"),
        "a browser was launched with stdout redirected: {text:?}"
    );
    assert!(
        text.contains("token="),
        "the printed URL carries no token, so it would not open: {text:?}"
    );
    assert!(
        !text.contains("THE MODEL WAS REACHED"),
        "`/web` was sent to the model: {text:?}"
    );
}

/// Asking twice reports where the view already is, rather than opening a second one.
///
/// Two listeners would be a quiet failure: the second bind succeeds on a second port, a
/// second URL is printed, and the page a person already has open is on neither.
#[tokio::test]
async fn the_web_command_twice_opens_one_listener() {
    let server = marker_provider("THE MODEL WAS REACHED").await;
    let home = test_home("web-twice", &server.uri());

    let text = repl(&home, &["/web", "/web", "/exit"]);
    let _ = std::fs::remove_dir_all(&home);

    let urls: Vec<&str> = text
        .lines()
        .filter_map(|line| line.split_once("web: ").map(|(_, url)| url.trim()))
        .collect();
    assert_eq!(urls.len(), 2, "expected two URLs, one per `/web`: {text:?}");
    assert_eq!(
        urls[0], urls[1],
        "the second `/web` opened a different listener: {text:?}"
    );
}

/// The guard catches an exact flag and nothing else.
///
/// This is the design, not an accident of the check: a line that merely *contains* a flag is
/// an ordinary question, and a pasted bullet list starts with a dash. A rule over anything
/// beginning with `-` was the obvious version and would swallow both.
#[tokio::test]
async fn a_sentence_about_a_flag_is_still_a_message() {
    let server = marker_provider("THE MODEL WAS REACHED").await;
    let home = test_home("flag-sentence", &server.uri());

    // One line per run, deliberately. With piped stdin every line is already in the pipe, so
    // the second arrives while the first turn is running and is delivered as *steering* --
    // which interrupts it. That is what typing during a turn is supposed to do, and it is why
    // these are two processes rather than one script.
    let sentence = repl(&home, &["why does --web need a token?"]);
    let bullet = repl(&home, &["- a pasted bullet"]);
    let _ = std::fs::remove_dir_all(&home);

    for (what, text) in [("a sentence about a flag", sentence), ("a bullet", bullet)] {
        assert!(
            !text.contains("is a start-up flag"),
            "the guard fired on {what}: {text:?}"
        );
        assert!(
            text.contains("THE MODEL WAS REACHED"),
            "{what} did not reach the model: {text:?}"
        );
    }
}

/// A busy port is reported, and the conversation survives it.
///
/// `/web` is not `--web`: there the view was the whole point of the run, so failing to bind is
/// fatal. Here it is one thing the user asked for, and taking a conversation down over a busy
/// port would be the worse answer.
#[tokio::test]
async fn the_web_command_survives_a_port_it_cannot_have() {
    let server = marker_provider("THE MODEL WAS REACHED").await;
    let home = test_home("web-busy", &server.uri());

    // Hold a port, then ask `/web` for it. Port 1 needs privilege and is refused for a
    // reason that has nothing to do with being busy, so a listener of our own is used.
    let held = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind a decoy");
    let port = held.local_addr().expect("addr").port();

    let text = repl(&home, &[&format!("/web {port}"), "hello"]);
    let _ = std::fs::remove_dir_all(&home);

    assert!(
        text.contains("cannot listen on 127.0.0.1"),
        "the refused port was not reported: {text:?}"
    );
    assert!(
        text.contains("THE MODEL WAS REACHED"),
        "the conversation did not survive a refused port: {text:?}"
    );
}

/// Run the REPL with these lines on stdin, and give up rather than blocking forever.
///
/// Same as `repl`, except that a process which never exits is a *failure* instead of a hang.
/// Output goes to a file rather than a pipe for the same reason: a child blocked writing into a
/// full pipe that nobody is draining looks exactly like a child that is stuck.
fn repl_within(home: &std::path::Path, lines: &[&str], secs: u64) -> (String, bool) {
    use std::io::Write;
    let out_path = home.join("stdout.txt");
    let file = std::fs::File::create(&out_path).expect("create the output file");
    let mut child = binary()
        .env("FLINT_HOME", home)
        .env_remove("NO_COLOR")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::from(file))
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("failed to run flint");
    {
        let stdin = child.stdin.as_mut().expect("no stdin handle");
        stdin
            .write_all(format!("{}\n", lines.join("\n")).as_bytes())
            .expect("failed to write stdin");
    }
    // Dropping the handle closes the pipe, which is what ends the input.
    drop(child.stdin.take());

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    let exited = loop {
        match child.try_wait().expect("try_wait") {
            Some(_) => break true,
            None if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                break false;
            }
            None => std::thread::sleep(std::time::Duration::from_millis(50)),
        }
    };
    let _ = child.wait();
    let text = std::fs::read_to_string(&out_path).unwrap_or_default();
    (text, exited)
}

/// The end of input has to end the REPL, even when a turn ate the message that said so.
///
/// This is a regression test for a defect the browser introduced, and the shape of it is worth
/// keeping: piping two lines gives `[Line, Line, Quit]` on the input channel. The first starts a
/// turn; the second arrives while it is running and is delivered as *steering*, which is what
/// typing during a turn is for. The third -- the `Quit` the reader sends when the pipe closes --
/// is then consumed by the turn the steering line started, and `run_turn` deliberately drops it,
/// because a pipe closing is not a person asking to stop.
///
/// That was harmless while the channel closed by itself when the reader thread ended: the REPL
/// left on `Disconnected`. Putting a second producer on the channel -- the page -- keeps it open
/// for the life of the process, so a dropped `Quit` became a process that waited forever for
/// input that could not come. Observed against a dead endpoint as `error: no network ...` and
/// then nothing, for as long as anyone was willing to watch.
#[tokio::test]
async fn the_end_of_input_ends_the_repl_even_when_a_turn_consumed_the_quit() {
    let server = marker_provider("THE MODEL WAS REACHED").await;
    let home = test_home("eof-steering", &server.uri());

    let (text, exited) = repl_within(&home, &["first question", "second question"], 30);
    let _ = std::fs::remove_dir_all(&home);

    assert!(
        exited,
        "flint never exited after its input ended: {text:?}"
    );
    // Both halves of the setup, so the test cannot pass by never reaching the case: the second
    // line has to have arrived as steering, and the turn it started has to have finished.
    assert!(
        text.contains("second question"),
        "the second line never became the next prompt, so no turn was running to eat the \
         `Quit`: {text:?}"
    );
    assert!(
        text.contains("THE MODEL WAS REACHED"),
        "the turn after the steering line did not finish: {text:?}"
    );
}

/// A line meant for flint must not be handed to the model just because a turn was running.
///
/// `run_turn` is where a line arriving mid-turn is picked up, and it has no way to run a
/// command: it takes the line and makes it the next prompt. So `/resume` typed -- or clicked in
/// the sidebar of the browser view -- while the model was working became a *message*, and the
/// conversation did not switch. Measured against a slow stub provider: `/resume 1` arrived, the
/// terminal drew `> /resume 1` as a prompt, and `/session` never changed.
///
/// That is the ordinary case, not a corner: the moment you want to look at another conversation
/// is while one is churning.
#[tokio::test]
async fn a_command_typed_during_a_turn_is_run_and_not_sent_to_the_model() {
    let server = marker_provider("THE MODEL WAS REACHED").await;
    let home = test_home("steer-command", &server.uri());

    // The second line arrives while the first turn is running, and is a command.
    let (text, exited) = repl_within(&home, &["a question", "/name from mid-turn"], 30);
    let _ = std::fs::remove_dir_all(&home);

    assert!(exited, "flint did not exit: {text:?}");
    assert!(
        text.contains("named: from mid-turn"),
        "the command was not run as a command: {text:?}"
    );
    assert!(
        !text.contains("> /name from mid-turn"),
        "the command was echoed as a prompt to the model: {text:?}"
    );
}

/// A provider that draws an answer and then holds the connection open.
///
/// `wiremock` cannot express this: a stub body is delivered whole, so the turn ends the
/// moment the answer does and there is no window in which to interrupt it. This writes the
/// deltas, flushes them, and keeps the response open -- which is what an interrupted turn
/// looks like from the server's side, and what makes "the answer has been drawn" a fact the
/// test waits for instead of a race it hopes to win. Every request body is kept, so the
/// test can read what the model was actually sent.
struct HangingProvider {
    base_url: String,
    bodies: std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
}

impl HangingProvider {
    fn start(drawn: &'static str) -> Self {
        use std::io::Write;

        let listener =
            std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind a stub provider");
        let base_url = format!("http://{}", listener.local_addr().expect("addr"));
        let bodies = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = bodies.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut sock) = stream else { continue };
                let seen = seen.clone();
                // One thread per connection: the first one is held open on purpose, and a
                // sequential loop would hold the second request behind it.
                std::thread::spawn(move || {
                    let body = read_http_body(&mut sock);
                    let first = {
                        let mut seen = seen.lock().expect("bodies");
                        seen.push(body);
                        seen.len() == 1
                    };
                    // `connection: close` puts the two requests on two sockets, so the
                    // first can be abandoned without the second landing on a half-read one.
                    let _ = sock.write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n",
                    );
                    if first {
                        // `{:?}` on a Rust string is a JSON string for anything ASCII.
                        let _ = sock.write_all(
                            format!(
                                "data: {{\"choices\":[{{\"delta\":{{\"content\":{drawn:?}}}}}]}}\n\n"
                            )
                            .as_bytes(),
                        );
                        let _ = sock.flush();
                        // Held open: the turn stays in flight until the client gives up.
                        std::thread::sleep(std::time::Duration::from_secs(20));
                    } else {
                        let _ = sock.write_all(concat!(
                            "data: {\"choices\":[{\"delta\":{\"content\":\"SECOND ANSWER\"}}]}\n\n",
                            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                            "data: [DONE]\n\n"
                        ).as_bytes());
                        let _ = sock.flush();
                    }
                });
            }
        });
        HangingProvider { base_url, bodies }
    }

    fn bodies(&self) -> Vec<String> {
        self.bodies
            .lock()
            .expect("bodies")
            .iter()
            .map(|b| String::from_utf8_lossy(b).to_string())
            .collect()
    }
}

/// Read one HTTP request, and return only its body.
fn read_http_body(sock: &mut std::net::TcpStream) -> Vec<u8> {
    use std::io::Read;
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    let (head_end, want) = loop {
        let n = match sock.read(&mut chunk) {
            Ok(0) | Err(_) => return Vec::new(),
            Ok(n) => n,
        };
        buf.extend_from_slice(&chunk[..n]);
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..end]).to_lowercase();
            let len = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            break (end + 4, len);
        }
    };
    while buf.len() < head_end + want {
        let n = match sock.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        buf.extend_from_slice(&chunk[..n]);
    }
    buf[head_end..].to_vec()
}

/// Start flint against the hanging provider and ask for an article, returning once the
/// answer is *on screen*.
///
/// Waiting for the drawn text rather than for a duration is what makes the interrupt land at
/// a known point instead of a hoped-for one: the fact under test is "the answer has been
/// drawn", and until it has been, there is nothing for an interrupt to lose.
fn mid_answer(home: &std::path::Path) -> (std::process::Child, std::process::ChildStdin, std::path::PathBuf) {
    use std::io::Write;

    let out_path = home.join("stdout.txt");
    let file = std::fs::File::create(&out_path).expect("stdout file");
    let mut child = binary()
        .env("FLINT_HOME", home)
        .env_remove("NO_COLOR")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::from(file))
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("failed to run flint");
    let mut stdin = child.stdin.take().expect("no stdin handle");
    stdin
        .write_all(b"write me an article\n")
        .expect("failed to write stdin");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let seen = std::fs::read_to_string(&out_path).unwrap_or_default();
        if seen.contains("A HALF-WRITTEN ARTICLE") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the answer was never drawn, so nothing was interrupted: {seen:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    (child, stdin, out_path)
}

/// The messages of the `n`th request the stub was sent, as one string.
fn sent_to_the_model(bodies: &[String], n: usize) -> String {
    let body: serde_json::Value =
        serde_json::from_str(&bodies[n]).expect("a request body is JSON");
    serde_json::to_string(&body["messages"]).expect("messages")
}

/// An interrupted turn keeps the answer it had already drawn.
///
/// Reported from a real session, and the report was exact: a model was streaming an article,
/// `/stop` was typed, and the next thing said was "finish writing it" -- answered by a model
/// that had no record of a word of it. The answer is drawn while the step is in flight and
/// only becomes a message when that step *completes*, so an interrupt -- which is a dropped
/// future -- drops the answer with it. What is on screen and not in the history is a
/// conversation the user and the model disagree about, and the user is the one who is right.
#[tokio::test]
async fn a_stopped_turn_keeps_the_answer_it_drew() {
    use std::io::Write;

    let provider = HangingProvider::start("A HALF-WRITTEN ARTICLE\n");
    let home = test_home("stop-keeps-drawn", &provider.base_url);
    let (mut child, mut stdin, out_path) = mid_answer(&home);

    // The stop, and then the line that refers to what was on screen.
    stdin
        .write_all(b"/stop\nfinish writing it\n")
        .expect("failed to write stdin");
    drop(stdin);

    let exited = wait_for_exit(&mut child, 20);
    let text = std::fs::read_to_string(&out_path).unwrap_or_default();
    let bodies = provider.bodies();
    // The session file too: a message that only ever existed in memory is one a restart
    // loses, and `/resume` is how a conversation is picked up later.
    let written: String = std::fs::read_dir(home.join("sessions"))
        .expect("the sessions directory")
        .filter_map(|entry| entry.ok())
        .map(|entry| std::fs::read_to_string(entry.path()).unwrap_or_default())
        .collect();
    let _ = std::fs::remove_dir_all(&home);

    assert!(exited, "flint did not exit: {text:?}");
    assert_eq!(
        bodies.len(),
        2,
        "expected the stopped turn and the one after it: {bodies:?}"
    );
    let sent = sent_to_the_model(&bodies, 1);
    assert!(
        sent.contains("A HALF-WRITTEN ARTICLE"),
        "the answer that was on screen is missing from what the model is sent: {sent}"
    );
    assert!(
        sent.contains("finish writing it"),
        "the line that referred to it is missing: {sent}"
    );
    assert!(
        written.contains("A HALF-WRITTEN ARTICLE"),
        "the answer was drawn but never written to the session file: {written}"
    );
}

/// The same guarantee through the other door: a line typed mid-turn *is* the interrupt.
///
/// This is the ordinary way to stop a turn -- the help text says "type while it works to
/// interrupt it" -- and it reaches the same commit by the same route: the line ends the turn
/// the same way `/stop` does, and the answer already drawn is still an answer.
#[tokio::test]
async fn a_steered_turn_keeps_the_answer_it_drew() {
    use std::io::Write;

    let provider = HangingProvider::start("A HALF-WRITTEN ARTICLE\n");
    let home = test_home("steer-keeps-drawn", &provider.base_url);
    let (mut child, mut stdin, _out_path) = mid_answer(&home);

    // No `/stop`: the line itself interrupts the turn and becomes the next prompt.
    stdin
        .write_all(b"finish writing it\n")
        .expect("failed to write stdin");
    drop(stdin);

    let exited = wait_for_exit(&mut child, 20);
    let bodies = provider.bodies();
    let _ = std::fs::remove_dir_all(&home);

    assert!(exited, "flint did not exit");
    assert_eq!(
        bodies.len(),
        2,
        "expected the interrupted turn and the steered one: {bodies:?}"
    );
    let sent = sent_to_the_model(&bodies, 1);
    assert!(
        sent.contains("A HALF-WRITTEN ARTICLE"),
        "the answer that was on screen is missing from the steered turn: {sent}"
    );
    assert!(
        sent.contains("finish writing it"),
        "the steering line is missing: {sent}"
    );
}

/// Wait for a spawned child to exit, killing it rather than hanging the suite.
fn wait_for_exit(child: &mut std::process::Child, secs: u64) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        match child.try_wait().expect("try_wait") {
            Some(_) => break true,
            None if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                break false;
            }
            None => std::thread::sleep(std::time::Duration::from_millis(50)),
        }
    }
}
