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
