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
