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
/// A UTF-8 em dash (`e2 80 94`) read as CP936 and written back out became `鈥?`, which
/// shipped in fifteen user-visible strings -- including the line a user sees the moment
/// they start a session read-only. Later, `" 用户"` in a test became `" 鐢ㄦ埛"`.
///
/// The build notices nothing, because the damage is valid UTF-8 either way: the file
/// compiles and the tests pass, and the corruption is only visible on screen. So this
/// checks the *text*.
///
/// The character set below is how CP936 renders UTF-8 three-byte sequences. Those code
/// points are real Chinese characters, but they are vanishingly rare in ordinary prose,
/// so finding one inside a string or comment means something was mis-decoded. Matching
/// on a fixed list of complete corrupted strings is not enough -- that was the first
/// version of this test, and it missed `鐢ㄦ埛` entirely.
#[test]
fn the_source_tree_contains_no_mojibake() {
    // Characters CP936 produces when it swallows a UTF-8 multi-byte sequence. Any of
    // these in this repository is an artifact, not prose.
    const MARKERS: &[char] = &[
        '\u{9225}', // 鈥  -- half of the em dash above
        '\u{9429}', '\u{951b}', '\u{9422}', // 锟 锛 鐢
        '\u{3126}', '\u{57db}', // ㄦ 埛 -- pieces of 用户
        '\u{8def}', // 路 -- a middle dot (U+00B7) mis-read as CP936. Ordinary-looking
                     // Chinese, which is exactly why it survived a scan for obvious
                     // garbage; it is listed because this repository has no prose that
                     // would use it. 败/失/项 are NOT listed for that reason: "项失败"
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
/// still painted `── 0s <tool> ──`, because committing a transcript line repainted the
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
        stdin
            .write_all(b"/resume 111-1\nhello\n/exit\n")
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
