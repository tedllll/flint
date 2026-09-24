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

/// One file, nothing behind it: the export a person can hand to somebody else.
///
/// The page is already a renderer for a finished conversation -- `--web` serves it, and dropping a
/// `.jsonl` on it draws one -- but neither is an artifact. The first needs a process and the second
/// needs two files and a drag. The export is those welded into one file, and the interesting half of
/// it is what it must *not* carry: the author's directory, a request to anywhere, and a conversation
/// that can reach out of the JSON island it is written into.
#[test]
fn an_exported_conversation_is_one_file_with_nothing_behind_it() {
    let home = test_home("export", "http://127.0.0.1:1/v1");
    let sessions = home.join("sessions");
    let worked_in = home.join("somewhere-on-my-machine");
    write_session(
        &sessions,
        "111-1.jsonl",
        &[
            &meta_line_in("111-1", &worked_in),
            r#"{"type":"chat","message":{"role":"user","content":"what changed in the queue"}}"#,
            r#"{"type":"chat","message":{"role":"assistant","content":"A follow-up waits."}}"#,
            r#"{"type":"title","name":"the queue"}"#,
        ],
        10,
    );
    let page = home.join("page.html");

    let out = binary()
        .current_dir(&home)
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .arg("export")
        .arg("111-1")
        .arg("--out")
        .arg(&page)
        .output()
        .expect("failed to run flint");

    assert!(
        out.status.success(),
        "export failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(
        said.contains("page.html"),
        "the page went to a file, so stdout has one line naming it: {said:?}"
    );
    assert_eq!(escape_count(&out.stdout), 0, "the export leaked escape codes");

    let html = std::fs::read_to_string(&page).expect("the exported page");
    assert!(
        html.contains("what changed in the queue") && html.contains("A follow-up waits."),
        "the conversation is not in the exported page"
    );
    assert!(
        html.contains("<!doctype html") || html.contains("<!DOCTYPE html"),
        "the exported page is not a page: {}",
        &html[..html.len().min(200)]
    );
    // What an artifact may not carry. The directory the conversation was held in is the author's
    // machine rather than the conversation, and a page that fetches something is not one file.
    assert!(
        !html.contains(&worked_in.display().to_string()),
        "the export names the directory the conversation was held in"
    );
    assert!(
        !html.contains("http://") && !html.contains("https://"),
        "the export reaches off the machine for something"
    );
    // The conversation is the file's own lines, in the island the page reads, and the page's headline
    // is the conversation's name rather than every export carrying the same browser tab.
    let island = island_of(&html);
    let parsed: serde_json::Value = serde_json::from_str(&island).expect("the island is JSON");
    let lines: Vec<String> = parsed["lines"]
        .as_array()
        .expect("the island carries lines")
        .iter()
        .map(|line| line.as_str().expect("a line is a string").to_string())
        .collect();
    assert!(
        lines.iter().any(|line| line.contains("what changed in the queue"))
            && lines.iter().any(|line| line.contains(r#""type":"meta""#)),
        "the island is not the session's own lines: {lines:?}"
    );
    assert!(
        !island.contains("somewhere-on-my-machine"),
        "the island still names the directory: {island}"
    );
    assert!(
        html.contains("<title>flint \u{2014} the queue</title>") || html.contains("the queue</title>"),
        "the exported page's title is not the conversation's name: {}",
        &html[..html.len().min(400)]
    );
}

/// A conversation cannot become script in the file it is exported to.
///
/// The conversation is written into the page as a JSON island inside a `<script>` element, and a
/// `<script>` element ends at the first `</script` in its text -- whoever wrote it. Tool output is
/// somebody else's text: a file that contains that sequence, or a model that was asked about it,
/// would otherwise truncate the island and leave the rest of the conversation to be parsed as HTML,
/// where `<script>alert(1)</script>` is a script. The island escapes `<`, `>` and `&`, so the bytes
/// that end an element never appear in it, and what the page reads back is the conversation exactly.
#[test]
fn a_conversation_line_cannot_become_script_in_an_export() {
    let home = test_home("export-script", "http://127.0.0.1:1/v1");
    let sessions = home.join("sessions");
    let nasty = r#"</script><script>alert(1)</script>"#;
    write_session(
        &sessions,
        "111-1.jsonl",
        &[
            &meta_line("111-1"),
            &format!(
                r#"{{"type":"chat","message":{{"role":"user","content":"{nasty}"}}}}"#
            ),
        ],
        10,
    );
    let page = home.join("page.html");
    let out = binary()
        .current_dir(&home)
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .arg("export")
        .arg("111-1")
        .arg("--out")
        .arg(&page)
        .output()
        .expect("failed to run flint");
    assert!(
        out.status.success(),
        "export failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let html = std::fs::read_to_string(&page).expect("the exported page");
    // Two real closers: the page's own script and the island's. The conversation's is escaped, so it
    // is not one of them -- which is the whole claim.
    assert_eq!(
        html.matches("</script>").count(),
        2,
        "a conversation line closed the script element early: {html}"
    );
    assert!(
        !html.contains("<script>alert(1)"),
        "the conversation was parsed as markup: {html}"
    );
    // And it comes back exactly, which is what makes this escaping rather than mangling.
    let island = island_of(&html);
    let parsed: serde_json::Value = serde_json::from_str(&island).expect("the island is JSON");
    assert!(
        parsed["lines"]
            .as_array()
            .expect("lines")
            .iter()
            .any(|line| line.as_str().unwrap_or_default().contains(nasty)),
        "the conversation did not survive the island"
    );
}

/// With no `--out`, the page is the whole of stdout and nothing else is: a caller redirects it.
#[test]
fn an_export_with_no_out_is_the_page_on_stdout() {
    let home = test_home("export-stdout", "http://127.0.0.1:1/v1");
    let sessions = home.join("sessions");
    write_session(
        &sessions,
        "111-1.jsonl",
        &[
            &meta_line("111-1"),
            r#"{"type":"chat","message":{"role":"user","content":"the question on stdout"}}"#,
        ],
        10,
    );

    let out = binary()
        .current_dir(&home)
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .args(["export", "111-1"])
        .output()
        .expect("failed to run flint");

    assert!(
        out.status.success(),
        "export failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let html = String::from_utf8(out.stdout).expect("the page is text");
    assert!(
        html.trim_start().starts_with("<!doctype html"),
        "stdout is not the page from its first byte: {}",
        &html[..html.len().min(200)]
    );
    assert!(
        html.contains("the question on stdout"),
        "the conversation is not on stdout"
    );
    assert_eq!(escape_count(html.as_bytes()), 0, "the export leaked escape codes");
}

/// A byte-order mark is the encoding's business, and an export may not keep the directory in one.
///
/// This is the case a Windows user actually has: `notepad` writes utf-8 *with* a mark, and so does
/// `Set-Content -Encoding utf8`. A JSON parser stops at the mark, so the `meta` line reads as damage
/// -- and a `meta` line carried as damage is a line whose `cwd` was never taken out. The export
/// writes a fresh utf-8 document with its own `<meta charset>`, so the mark is not content to carry,
/// and the island it writes must be readable without one.
#[test]
fn an_export_of_a_marked_session_still_leaves_the_directory_out() {
    let home = test_home("export-bom", "http://127.0.0.1:1/v1");
    let sessions = home.join("sessions");
    let worked_in = home.join("somewhere-on-my-machine");
    let path = sessions.join("111-1.jsonl");
    std::fs::write(
        &path,
        format!(
            "\u{feff}{}\n{}\n",
            meta_line_in("111-1", &worked_in),
            r#"{"type":"chat","message":{"role":"user","content":"a question"}}"#
        ),
    )
    .expect("write session");

    let out = binary()
        .current_dir(&home)
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .args(["export", "111-1"])
        .output()
        .expect("failed to run flint");
    assert!(
        out.status.success(),
        "export failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let html = String::from_utf8(out.stdout).expect("the page is text");
    assert!(
        !html.contains(&worked_in.display().to_string()),
        "the mark hid the meta line from the parser, and its directory was carried with it"
    );
    let island = island_of(&html);
    let parsed: serde_json::Value = serde_json::from_str(&island).expect("the island is JSON");
    let first = parsed["lines"][0].as_str().expect("the meta line is a string");
    assert!(
        first.starts_with('{') && first.contains(r#""type":"meta""#),
        "the island's first line is not the readable meta line: {first:?}"
    );
}

/// The refusals: a session that is not there, and a `--out` with nothing to write.
#[test]
fn an_export_refuses_what_it_cannot_do() {
    let home = test_home("export-refusals", "http://127.0.0.1:1/v1");

    let missing = binary()
        .current_dir(&home)
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .args(["export", "no-such-session"])
        .output()
        .expect("failed to run flint");
    assert!(!missing.status.success(), "a missing session was exported");
    let stderr = String::from_utf8_lossy(&missing.stderr);
    assert!(
        stderr.contains("no-such-session"),
        "the refusal does not name what was asked for: {stderr}"
    );

    let stray = binary()
        .current_dir(&home)
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .args(["--out", "somewhere.html", "-p", "hi"])
        .output()
        .expect("failed to run flint");
    assert!(!stray.status.success(), "--out was accepted with nothing to export");
    let stderr = String::from_utf8_lossy(&stray.stderr);
    assert!(
        stderr.contains("--out"),
        "the refusal does not name the flag: {stderr}"
    );
}

/// The JSON island out of an exported page, as text.
fn island_of(html: &str) -> String {
    let start = html
        .find(r#"<script id="session" type="application/json">"#)
        .expect("the exported page carries an island");
    let after = &html[start..];
    let open = after.find('>').expect("the island's opening tag") + 1;
    let end = after.find("</script>").expect("the island's closing tag");
    after[open..end].to_string()
}

/// A conversation with no messages in it is not worth a page, and saying so beats writing one.
#[test]
fn an_export_of_an_empty_session_is_refused() {
    let home = test_home("export-empty", "http://127.0.0.1:1/v1");
    let sessions = home.join("sessions");
    write_session(&sessions, "111-1.jsonl", &[&meta_line("111-1")], 10);

    let out = binary()
        .current_dir(&home)
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .args(["export", "111-1"])
        .output()
        .expect("failed to run flint");

    assert!(!out.status.success(), "an empty conversation was exported");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("111-1"),
        "the refusal does not name the session: {stderr}"
    );
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
/// A one-shot run with a budget stops when the budget is gone, not when the provider answers.
///
/// The plain path, not `--json`: a shell caller with no stream to read still needs the bound and gets
/// only the exit code, so the code has to carry it. This is the case the flag exists for -- a request
/// that never comes back, which no step limit can cut and whose wait is not flint's to sit out.
#[tokio::test]
async fn a_one_shot_run_stops_when_its_budget_is_gone() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse(&[
                    r#"data: {"choices":[{"delta":{"content":"TOO LATE"}}]}"#,
                    "data: [DONE]",
                ]))
                .set_delay(std::time::Duration::from_secs(30)),
        )
        .mount(&server)
        .await;

    let dir = std::env::temp_dir().join(format!("flint-cli-budget-{}", std::process::id()));
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

    let began = std::time::Instant::now();
    let out = binary()
        .args(["-p", "think about it", "--max-seconds", "1"])
        .env("FLINT_HOME", &dir)
        .env_remove("NO_COLOR")
        .output()
        .expect("failed to run flint");
    let waited = began.elapsed();
    let _ = std::fs::remove_dir_all(&dir);
    let printed = String::from_utf8_lossy(&out.stdout).to_string();
    // The reason goes to stderr on the plain path, which is where a non-interactive run's notices
    // have always gone: stdout is the answer, and nothing else may be on it.
    let noted = String::from_utf8_lossy(&out.stderr).to_string();

    assert!(
        waited < std::time::Duration::from_secs(15),
        "the run waited {waited:?} for an answer the caller had already given up on"
    );
    assert_eq!(
        out.status.code(),
        Some(65),
        "a run cut short by its budget exited as though it had answered: {printed} / {noted}"
    );
    assert!(
        noted.contains("1-second budget"),
        "nothing said what ended the run: {noted}"
    );
    assert!(
        !printed.contains("TOO LATE"),
        "an answer that arrived after the deadline was printed as though it were in time"
    );
}

/// The mirror of the budget: a flag about *between* turns has no turn to act in when there is one.
///
/// Refused rather than ignored, because the failure this prevents is the quiet one: a caller who
/// believed a peer's words would be relayed, and a run that read no mailbox at all.
#[test]
fn hearing_peers_on_a_one_shot_run_is_refused() {
    let (code, out) = run(&["-p", "hello", "--hear-peers"]);
    assert_eq!(code, 2, "{}", String::from_utf8_lossy(&out));
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.contains("--hear-peers") && text.contains("/hear-peers"),
        "the refusal does not name the flag or the command that would work: {text}"
    );
}

/// The same flag on a run with no prompt: there is no call to bound, and a budget is not a setting.
#[test]
fn a_budget_without_a_prompt_is_refused() {
    let (code, out) = run(&["--max-seconds", "5"]);
    assert_eq!(code, 2, "{}", String::from_utf8_lossy(&out));
    assert!(
        String::from_utf8_lossy(&out).contains("--max-seconds"),
        "the refusal does not name the flag: {}",
        String::from_utf8_lossy(&out)
    );
}

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

/// Non-ASCII a scanned file may always contain: the punctuation and symbols this repository's
/// English prose and its terminal tests are written with.
///
/// Ranges rather than a list of characters, so that typing an ellipsis does not fail the build
/// for the wrong reason, and so the rule can be read as a *class*: punctuation, symbols, box
/// drawing and emoji, and no writing system at all.
fn always_allowed(c: char) -> bool {
    matches!(
        c as u32,
        0x00A0..=0x00FF // Latin-1: § · × ¥ © « » and the accented letters in the code
            | 0x2000..=0x206F // general punctuation: — – … ‹ › and the curly quotes
            | 0x2070..=0x209F // super- and subscripts
            | 0x20A0..=0x20CF // currency signs
            | 0x2190..=0x21FF // arrows: ← → ↔
            | 0x2200..=0x22FF // mathematical operators: ≡ ⋯
            | 0x2300..=0x23FF // miscellaneous technical: the transcript's own marks, ⏵ while a tool
                              // runs and ⏹ for a turn that was interrupted. The source writes them
                              // as `\u{23f5}`-style escapes, so this is the first file to spell them:
                              // `docs/features.md` quotes what a person sees on screen, and a
                              // reader checking a build has to be able to recognise the glyph
            | 0x2500..=0x257F // box drawing, which the terminal fixtures draw with
            | 0x25A0..=0x27BF // geometric shapes and dingbats: ✓ ✗ ✔ ⚠
            | 0x2E00..=0x2E7F // supplemental punctuation
            | 0xFE00..=0xFE0F // variation selectors, as in a warning sign with an emoji look
            | 0x1F000..=0x1FAFF // emoji
    )
}

/// A character from a writing system this repository writes in *deliberately*, and only in the
/// files named below.
///
/// This is the second half of a whitelist, and the half that makes it worth having. The
/// artifacts of a CP936 round trip (`鈥`, `璺`, `鍩`) are ordinary CJK ideographs, so no rule of
/// the form "these ideographs mean damage" can be complete: it is a list of accidents, and the
/// second accident over the first produces characters the list has never seen. The rule here is
/// a different kind of statement -- **CJK belongs to a file, not to a character** -- so a file
/// that holds Chinese text says so, in the table below, with a reason; and every other file may
/// hold no ideograph at all. That is where damage is hardest to see, because it reads as text.
fn cjk(c: char) -> bool {
    matches!(
        c as u32,
        0x3000..=0x303F // CJK punctuation: 。 、 （ ） and the fullwidth colon
            | 0x3040..=0x30FF // kana
            | 0x3400..=0x4DBF // unified ideographs, extension A
            | 0x4E00..=0x9FFF // unified ideographs
            | 0xF900..=0xFAFF // compatibility ideographs
            | 0xFF00..=0xFFEF // fullwidth and halfwidth forms: ， （ ）
            | 0x20000..=0x2FFFF // extensions B and beyond, for completeness
    )
}

/// Every scanned file that may contain CJK, and why that file does. Paths are relative to the
/// repository root, with `/`, because that is how an offender is reported on both platforms.
///
/// The list is the point rather than a formality: it is what makes Chinese in a *new* file fail
/// the build until somebody writes down that it belongs there, and what keeps the one file that
/// has to name the artifacts (`tests/cli_output.rs`, the marker table) from being the file where
/// real damage hides. Adding a file here is a claim about its contents; the reason is the claim.
const CJK_FILES: &[(&str, &str)] = &[
    ("README.md", "quotes the questions a person asked, in the words they asked them"),
    ("ROADMAP.md", "quotes the reports this plan was built from, in the words they arrived in"),
    ("HANDOFF.md", "the same, for the reports the last sessions were built from"),
    ("docs/deepseek-search.md", "quotes DeepSeek's own description of the search endpoint"),
    ("docs/python.md", "a Python call written the way its author would write it"),
    ("docs/sandbox.md", "two characters quoted for how they feel, not for what they say"),
    ("docs/web-mode.md", "§13 quotes the complaint it was built from, in the words it arrived in"),
    ("docs/windows.md", "the console title Windows prints, in Chinese"),
    ("docs/windows-tooling.md", "the `你好` whose mangling *is* the measurement"),
    ("examples/channels.rs", "the Chinese prompt the example defaults to"),
    ("examples/live_turn.rs", "the command line a person would run the example with"),
    ("scripts/layout-trace.js", "the transcript it draws is Chinese"),
    ("scripts/term-layout-test.js", "the layout fixtures are Chinese, and width is measured in it"),
    ("src/fetch.rs", "one character, to prove an HTML entity decodes to it"),
    ("src/term.rs", "the width tests: a CJK character is two columns, which is the point"),
    ("src/tools.rs", "the `你好` a code page mangles, in the comment that says why"),
    ("src/util.rs", "`你好 café 日本語`, the truncation test's non-ASCII sample"),
    ("tests/agent_loop.rs", "the same mangling, asserted through a script"),
    ("tests/cli_output.rs", "the marker table, which has to name the characters it looks for"),
    ("tests/term_capture.rs", "the byte-exact terminal fixtures: Chinese prompts and answers"),
];

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
///
/// The extensions are the ones this repository writes text into, `.html` included: the page the
/// listener serves is where a person reads flint's words, so damage there is the same defect one
/// file over -- and it went unscanned for as long as this list did not name it. Measured
/// 2026-09-17: with a middle dot in `web/view.html` replaced by the CP936 artifact it decodes to,
/// this test passed while `"html"` was missing from the list and failed once it was added.
///
/// **A list of markers is a list of accidents, so the second check is a whitelist.** Two things
/// are true of the characters above and are the reason they cannot be the whole guard: they are
/// what *one* bad round trip produces, and a second round trip over already-damaged text produces
/// characters they have never held. That is not hypothetical either -- it is how this session
/// found the hole, by making some (`U+95B3`, `U+95BB`) in this very file while editing it through
/// a PowerShell pipeline, and watching the guard pass. So the tree is also checked the other way
/// round: every non-ASCII character in a scanned file has to be one this repository *means* --
/// either a punctuation or symbol class (`always_allowed`), or CJK in a file that is declared to
/// hold CJK text (`CJK_FILES`, each with a reason).
///
/// The two checks are not redundant. The marker list still earns its place *inside* a declared
/// file, where a whitelist cannot tell prose from damage; the whitelist is what refuses an
/// ideograph in the ~30 scanned files that hold none, which is every Rust source, every document
/// that quotes no Chinese, the page, and `Cargo.toml` -- the places damage is invisible because
/// nothing else there is non-ASCII.
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
    let mut unmeant: Vec<String> = Vec::new();
    let mut declared_seen: Vec<&str> = Vec::new();
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
                Some("rs" | "js" | "md" | "toml" | "yml" | "yaml" | "html")
            );
            if !is_text {
                continue;
            }
            scanned += 1;
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            // `/` on both platforms, because that is how a declaration is written and how an
            // offender is reported; a Windows path here would silently match nothing.
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let declared = CJK_FILES.iter().find(|(file, _)| *file == relative);
            if let Some((file, _)) = declared {
                declared_seen.push(file);
            }
            for (n, line) in text.lines().enumerate() {
                // This file has to name the characters it looks for, and the marker
                // definitions are the only lines where that is legitimate. Excluding the
                // whole file instead -- which this used to do -- leaves the one file that
                // is *about* encoding damage as the one file damage could hide in.
                if path.ends_with("cli_output.rs") && is_marker_definition(line) {
                    continue;
                }
                if MARKERS.iter().any(|m| line.contains(*m)) {
                    offenders.push(format!("{relative}:{}: {}", n + 1, line.trim()));
                }
                // The whitelist half: a character this repository does not mean is an artifact,
                // whatever list it is on. Reported with its code point, because a character that
                // is damage looks like text in the failure message otherwise.
                for c in line.chars().filter(|c| !c.is_ascii()) {
                    if always_allowed(c) || (declared.is_some() && cjk(c)) {
                        continue;
                    }
                    unmeant.push(format!(
                        "{relative}:{}: U+{:04X} `{c}` in: {}",
                        n + 1,
                        c as u32,
                        line.trim()
                    ));
                }
            }
        }
    }

    assert!(scanned > 5, "the walk found almost nothing ({scanned} files)");
    for (file, reason) in CJK_FILES {
        assert!(
            declared_seen.contains(file),
            "`{file}` is declared as a file that holds CJK ({reason}), and the walk never saw \
             it -- a renamed file leaves a declaration that now protects nothing"
        );
        assert!(
            !reason.trim().is_empty(),
            "`{file}` is declared as holding CJK with no reason, which is the form alone"
        );
    }
    assert!(
        offenders.is_empty(),
        "mojibake in the source tree -- a UTF-8 file was written back through a CP936 \
         code page:\n{}",
        offenders.join("\n")
    );
    assert!(
        unmeant.is_empty(),
        "non-ASCII this repository does not mean -- damage that no marker list holds, or a \
         writing system in a file that was never declared to hold one (`CJK_FILES`):\n{}",
        unmeant.join("\n")
    );
}

/// The example renders a turn with the REPL's own sink, not with a copy of it.
///
/// `examples/live_turn.rs` exists so a layout can be *judged* from real model output, which it does by
/// driving the same code the REPL drives. It used to keep a hand-written copy of `run_turn`'s event
/// handling, and the copy drifted twice: the example showed a transcript the REPL no longer produced
/// and, being a second implementation, hid the real one. Both cost time chasing a fault in the wrong
/// file. What keeps that from coming back is this: the example must call the sink, and must not match
/// on events itself.
#[test]
fn the_example_renders_with_the_repls_sink() {
    let example = std::fs::read_to_string("examples/live_turn.rs").expect("the example");
    assert!(
        example.contains("sink::EventSink") || example.contains("use flint::sink::EventSink"),
        "the example does not use the sink the REPL uses, so it is a second rendering of a turn and \
         a layout judged from it is judged against the wrong thing: {example}"
    );
    assert!(
        example.contains("|event| sink.event(event)"),
        "the example does not feed its turn to the sink, so whatever it renders is its own: \
         {example}"
    );
    for copied in ["Event::Text", "Event::ToolArgs", "Event::ToolResult", "match event"] {
        assert!(
            !example.contains(copied),
            "the example matches on `{copied}` itself, which is the copy of `run_turn`'s event \
             handling that drifted twice: {example}"
        );
    }
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

/// The same line, held in a directory of the caller's choosing.
///
/// Escaped, because this is JSON and a Windows path is nothing but backslashes: written raw,
/// `"cwd":"C:\Users\..."` is not JSON, the line is skipped as damage, and a test about which
/// directory a session belongs to would pass by skipping every session.
fn meta_line_in(id: &str, cwd: &std::path::Path) -> String {
    let cwd = cwd.display().to_string().replace('\\', "\\\\");
    format!(
        r#"{{"type":"meta","v":2,"id":"{id}","created":"epoch:1","cwd":"{cwd}","provider":"stub","model":"stub-model"}}"#
    )
}

/// `--cwd` is the directory the run works in, whatever the process's own directory is.
///
/// A program that drives flint one process per question does not start in the project it is asking
/// about -- it names it. Everything that makes a conversation findable later has to agree on that
/// directory: the tools run in it, the `meta` line records it, and `--continue` matches on it. So
/// this drives the binary from one directory with `--cwd` naming another, twice, and requires the
/// second run to continue the first one's conversation. Two different process directories are used,
/// because that is the case a caller that passes a *relative* path would otherwise lose: the
/// recorded path would be resolved against the next process's directory and match nothing.
#[tokio::test]
async fn a_run_works_in_the_directory_it_was_given() {
    let server = MockServer::start().await;
    answer_once(&server).await;
    let home = test_home("cwd-flag", &server.uri());
    let project = home.join("project");
    let elsewhere = home.join("elsewhere");
    let also_elsewhere = home.join("also-elsewhere");
    for dir in [&project, &elsewhere, &also_elsewhere] {
        std::fs::create_dir_all(dir).expect("directory");
    }

    // Run one: started in a directory that has nothing to do with the project.
    let first = binary()
        .current_dir(&elsewhere)
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .args(["--cwd"])
        .arg(&project)
        .args(["-p", "the first question"])
        .output()
        .expect("failed to run flint");
    assert!(
        first.status.success(),
        "flint --cwd failed: {}",
        String::from_utf8_lossy(&first.stderr)
    );

    let sessions = home.join("sessions");
    // The session is not in the sessions root but in the subdirectory that belongs to the working
    // directory, named after it with a hash of the whole path. That layout *is* the separation
    // between projects, so it is asserted rather than assumed -- a file left in the root is a file
    // no project owns, and it is what `--continue` would then have to guess about.
    let owned: Vec<std::path::PathBuf> = std::fs::read_dir(&sessions)
        .expect("read sessions")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    assert_eq!(owned.len(), 1, "one project, one directory: {owned:?}");
    let name = owned[0]
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string();
    assert!(
        name.starts_with("project-"),
        "the directory is not named after the project: {name}"
    );
    let files: Vec<std::path::PathBuf> = std::fs::read_dir(&owned[0])
        .expect("read the project's sessions")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("jsonl"))
        .collect();
    assert_eq!(files.len(), 1, "one run, one session: {files:?}");
    assert_eq!(
        std::fs::read_dir(&sessions)
            .expect("read sessions")
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("jsonl"))
            .count(),
        0,
        "a session was left in the root, where no project owns it"
    );
    let text = std::fs::read_to_string(&files[0]).expect("read the session");
    let meta = text.lines().next().expect("a meta line");
    let recorded: serde_json::Value = serde_json::from_str(meta).expect("the meta line is JSON");
    let recorded = recorded["cwd"].as_str().unwrap_or_default();
    // Compared canonicalised, because what is recorded is the path as the caller spelled it made
    // absolute -- deliberately not canonicalised, which on Windows means a verbatim `\\?\` prefix
    // in a file people read and edit by hand.
    let want = std::fs::canonicalize(&project).expect("canonical project");
    let got = std::fs::canonicalize(recorded).expect("canonical recorded cwd");
    assert_eq!(
        got, want,
        "the session did not record the directory the run was asked for: {meta}"
    );

    // Run two: a *different* process directory, the same project. The conversation must be found.
    let second = binary()
        .current_dir(&also_elsewhere)
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .args(["--cwd"])
        .arg(&project)
        .args(["--continue", "-p", "the second question"])
        .output()
        .expect("failed to run flint");
    assert!(
        second.status.success(),
        "flint --cwd --continue failed: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        stderr.contains("resumed"),
        "the conversation held in --cwd was not found from another process directory: {stderr}"
    );
    assert_eq!(
        std::fs::read_dir(&owned[0])
            .expect("read the project's sessions")
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("jsonl"))
            .count(),
        1,
        "continuing the conversation started a second one"
    );
    let after = std::fs::read_to_string(&files[0]).expect("read the session again");
    assert!(
        after.contains("the second question"),
        "the second run did not append to the conversation in --cwd: {after}"
    );
}

/// Two projects sharing one home keep their conversations apart -- in the layout, not in a filter.
///
/// This is the shape a program driving flint ends up in: one home, one process per question, one
/// directory per project. Nothing here reads `meta.cwd` to decide which conversation is whose; each
/// project has a directory of its own under `sessions/`, which is why the listing can still see all
/// of them and `--continue` can mean something exact.
#[tokio::test]
async fn two_projects_in_one_home_keep_their_conversations_apart() {
    let server = MockServer::start().await;
    answer_once(&server).await;
    let home = test_home("two-projects", &server.uri());
    let one = home.join("one");
    let two = home.join("two");
    std::fs::create_dir_all(&one).expect("one");
    std::fs::create_dir_all(&two).expect("two");

    for (dir, question) in [
        (&one, "the first project speaks"),
        (&two, "the second project speaks"),
    ] {
        let out = binary()
            .current_dir(&home)
            .env("FLINT_HOME", &home)
            .env_remove("NO_COLOR")
            .args(["--cwd"])
            .arg(dir)
            .args(["-p", question])
            .output()
            .expect("failed to run flint");
        assert!(
            out.status.success(),
            "--cwd {} failed: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let sessions = home.join("sessions");
    let mut dirs: Vec<String> = std::fs::read_dir(&sessions)
        .expect("read sessions")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .collect();
    dirs.sort();
    assert_eq!(dirs.len(), 2, "the projects were not separated: {dirs:?}");
    assert!(dirs[0].starts_with("one-"), "{dirs:?}");
    assert!(dirs[1].starts_with("two-"), "{dirs:?}");
    assert_eq!(
        std::fs::read_dir(&sessions)
            .expect("read sessions")
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("jsonl"))
            .count(),
        0,
        "a session was left in the root, where no project owns it"
    );

    // Each directory holds its own question and not the other's, so nothing was shared or crossed.
    for (name, mine, theirs) in [
        (
            &dirs[0],
            "the first project speaks",
            "the second project speaks",
        ),
        (
            &dirs[1],
            "the second project speaks",
            "the first project speaks",
        ),
    ] {
        let files: Vec<std::path::PathBuf> = std::fs::read_dir(sessions.join(name))
            .expect("read the project's sessions")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("jsonl"))
            .collect();
        assert_eq!(files.len(), 1, "{name} holds {files:?}");
        let text = std::fs::read_to_string(&files[0]).expect("read the session");
        assert!(text.contains(mine), "{name} does not hold its own question");
        assert!(
            !text.contains(theirs),
            "{name} holds the other project's question"
        );
    }

    // The listing is of the home, not of wherever it happens to be run from -- here, a directory
    // with no conversation of its own.
    let listed = binary()
        .current_dir(&home)
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .arg("--list-sessions")
        .output()
        .expect("failed to run flint");
    assert!(
        listed.status.success(),
        "{}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let listing = String::from_utf8_lossy(&listed.stdout);
    assert!(
        listing.contains("the first project") && listing.contains("the second project"),
        "the listing is not of the whole home: {listing}"
    );
}

/// The same listing, as data, for the caller that cannot read a printed line.
///
/// `--list-sessions` prints `N  id  label`, which a program can split -- but only by knowing a
/// format that exists for a person, and the label is a rule (a name, else the first thing said) that
/// a caller would then be re-implementing. The JSON form carries the fields the listing is made of,
/// including the session *path*, which is the internal detail `ROADMAP.md` §10 says a caller must not
/// have to reconstruct: a session is no longer necessarily directly in `sessions/`.
#[test]
fn the_session_list_has_a_machine_readable_form() {
    let home = test_home("sessions-json", "http://127.0.0.1:1/v1");
    let sessions = home.join("sessions");
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
        .args(["--list-sessions", "--json"])
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .output()
        .expect("failed to run flint");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "listing failed: {stderr}");

    // One object, on one line, with a `type`: the same rule every other `--json` answer follows, so
    // a reader that already handles `who --json` needs no new case.
    let mut lines = stdout.lines();
    let first = lines.next().expect("no listing at all");
    assert!(
        lines.next().is_none(),
        "the listing is more than one line of JSON: {stdout:?}"
    );
    let listing: serde_json::Value = serde_json::from_str(first).expect("the listing is not JSON");
    assert_eq!(listing["type"], "sessions");
    assert_eq!(listing["count"], 2, "the archived session is in the count: {listing}");
    let rows = listing["sessions"].as_array().expect("an array of sessions");
    assert_eq!(rows.len(), 2, "{listing}");

    // Newest first, and the index is the number `--resume` takes -- the same number the printed
    // listing puts in front of the same session, because both are the same list.
    assert_eq!(rows[0]["id"], "111-1", "{listing}");
    assert_eq!(rows[0]["index"], 1, "{listing}");
    assert_eq!(rows[1]["index"], 2, "{listing}");
    // The label rule lives in one place: a session with a name is labelled by it, one without by
    // what was said first.
    assert_eq!(rows[0]["label"], "codex config", "{listing}");
    assert_eq!(rows[0]["title"], "codex config", "{listing}");
    assert_eq!(rows[1]["label"], "update dsh please", "{listing}");

    // The path is the field a caller cannot reconstruct, and it has to be a file that is there.
    let path = rows[0]["path"].as_str().expect("a session path");
    assert!(
        std::path::Path::new(path).is_file(),
        "the path in the listing is not a file: {path}"
    );
    assert!(
        !stdout.contains("long forgotten"),
        "an archived session is in the JSON listing: {stdout:?}"
    );
    assert!(stderr.is_empty(), "the listing wrote to stderr: {stderr:?}");
}

/// `--cwd` naming something that is not a directory is refused, and nothing is created.
///
/// The alternative -- letting the run start and watching every tool fail -- reads as flint being
/// broken rather than as a typo in the caller, and creating the directory would leave one behind
/// from a run that never happened.
#[test]
fn a_cwd_that_is_not_a_directory_is_refused() {
    let home = test_home("cwd-missing", "http://127.0.0.1:1/v1");
    let missing = home.join("not-a-directory-at-all");

    let out = binary()
        .current_dir(&home)
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .args(["--cwd"])
        .arg(&missing)
        .args(["-p", "anything"])
        .output()
        .expect("failed to run flint");

    assert!(!out.status.success(), "--cwd with no such directory was accepted");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--cwd"),
        "the refusal must name the flag whose value was wrong: {stderr}"
    );
    assert!(!missing.exists(), "--cwd created the directory it was given");
    let sessions: Vec<_> = std::fs::read_dir(home.join("sessions"))
        .expect("read sessions")
        .filter_map(|e| e.ok())
        .collect();
    assert!(
        sessions.is_empty(),
        "a refused run left a session behind: {sessions:?}"
    );
}

/// `--continue` continues the conversation held in *this* directory, not the newest one anywhere.
///
/// It used to be the newest file in the home, which is the wrong answer for anyone with two
/// projects -- and wrong in silence: the run resumes the other project's history, appends to it, and
/// says nothing. One home with a directory per project is exactly the shape a program driving flint
/// -- one process per question -- is in, so this is the case where the wrong pick is least visible
/// and most damaging.
///
/// The assertion is on bytes, because that is the promise: the other directory's file must be
/// untouched, and this directory's file must have grown. The *newer* session is the one in the other
/// directory, so a regression to modification-time order fails this test rather than passing it by
/// luck.
#[tokio::test]
async fn continue_resumes_the_conversation_held_in_this_directory() {
    let server = MockServer::start().await;
    answer_once(&server).await;
    let home = test_home("continue-here", &server.uri());
    let sessions = home.join("sessions");
    let work = home.join("project");
    let elsewhere = home.join("elsewhere");
    std::fs::create_dir_all(&work).expect("working directory");
    std::fs::create_dir_all(&elsewhere).expect("another directory");

    let mine = write_session(
        &sessions,
        "111-1.jsonl",
        &[
            &meta_line_in("111-1", &work),
            r#"{"type":"chat","message":{"role":"user","content":"the question from this directory"}}"#,
            r#"{"type":"chat","message":{"role":"assistant","content":"an answer"}}"#,
        ],
        30,
    );
    let theirs = write_session(
        &sessions,
        "222-1.jsonl",
        &[
            &meta_line_in("222-1", &elsewhere),
            r#"{"type":"chat","message":{"role":"user","content":"a question from somewhere else"}}"#,
        ],
        0,
    );
    let theirs_before = std::fs::read(&theirs).expect("read the other directory's session");

    let out = binary()
        .current_dir(&work)
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .args(["--continue", "-p", "carry on"])
        .output()
        .expect("failed to run flint");
    assert!(
        out.status.success(),
        "flint --continue failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert_eq!(
        std::fs::read(&theirs).expect("read the other directory's session again"),
        theirs_before,
        "another directory's conversation was resumed into: --continue picked by time, not by place"
    );
    let mine_after = std::fs::read_to_string(&mine).expect("read this directory's session");
    assert!(
        mine_after.contains("carry on"),
        "this directory's conversation did not continue: {mine_after}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("111-1.jsonl"),
        "the run did not say which session it resumed: {stderr}"
    );
}

/// With nothing held here, `--continue` starts a conversation and says so.
///
/// Not reaching for another directory's conversation is the point; saying so is the other half.
/// A caller that asked to continue and got a fresh conversation in silence has lost the thread it
/// thought it was holding, which is worse than an error, because nothing looks wrong.
#[tokio::test]
async fn continue_with_nothing_here_starts_one_and_says_so() {
    let server = MockServer::start().await;
    answer_once(&server).await;
    let home = test_home("continue-empty", &server.uri());
    let sessions = home.join("sessions");
    let work = home.join("project");
    let elsewhere = home.join("elsewhere");
    std::fs::create_dir_all(&work).expect("working directory");
    std::fs::create_dir_all(&elsewhere).expect("another directory");

    let theirs = write_session(
        &sessions,
        "222-1.jsonl",
        &[
            &meta_line_in("222-1", &elsewhere),
            r#"{"type":"chat","message":{"role":"user","content":"a question from somewhere else"}}"#,
        ],
        0,
    );
    let theirs_before = std::fs::read(&theirs).expect("read the other directory's session");

    let out = binary()
        .current_dir(&work)
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .args(["--continue", "-p", "first question here"])
        .output()
        .expect("failed to run flint");
    assert!(out.status.success(), "flint failed: {}", String::from_utf8_lossy(&out.stderr));

    assert_eq!(
        std::fs::read(&theirs).expect("read the other directory's session again"),
        theirs_before,
        "the empty directory was given another directory's conversation"
    );
    // The other directory's conversation is where it was, and this one has a conversation of its
    // own: the flat file the other directory was given, plus the new one, in this project's own
    // subdirectory.
    let new_sessions = jsonl_files(&sessions);
    assert_eq!(
        new_sessions.len(),
        2,
        "a new conversation should have been started here: {new_sessions:?}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no session for"),
        "starting fresh in silence is the failure this test exists for: {stderr}"
    );
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
    let resume_args = jsonl_files(&home.join("sessions"))
        .into_iter()
        .next()
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
/// `/name` writes to the conversation that is open, and `/delete` on that same conversation is the
/// case this test is about.
///
/// It used to assert a *refusal*, and the refusal was the honest answer to a real problem: this
/// process appends to that file, so deleting it under the writer leaves the writer pointing at a path
/// that no longer exists, and the next event recreates it as a nameless fragment. What it was not is a
/// reason the person cannot have what they asked for -- the workaround it named (`/new` starts a fresh
/// one, then this one can be filed away by its number) *is* the operation, so the command does both.
/// Asked for directly on 2026-09-23, from the page: the sidebar's `...` draws the destructive rows on
/// the conversation being written like any other row, and a row that is offered and then refused is
/// worse than one that was never drawn.
///
/// Both halves are asserted, because either alone passes for the wrong reason: the terminal says what
/// happened, and the file that is left is the *new* conversation rather than a resurrected fragment of
/// the old one, which is the exact failure the refusal existed to prevent.
#[cfg(debug_assertions)]
#[tokio::test]
async fn deleting_the_open_session_starts_a_fresh_one_and_the_old_one_goes() {
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

    let home = test_home("repl-delete-open", &server.uri());
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
        // `1` is this run's own conversation: the listing numbers the newest first, and the run is
        // writing the newest there is.
        stdin
            .write_all(b"/name a named conversation\n/delete 1\nhello\n/exit\n")
            .expect("failed to write stdin");
    }
    let out = child.wait_with_output().expect("flint did not finish");
    let text = String::from_utf8_lossy(&out.stdout).to_string();

    let files = jsonl_files(&home.join("sessions"));
    let body = files
        .first()
        .map(|file| std::fs::read_to_string(file).unwrap_or_default())
        .unwrap_or_default();
    let _ = std::fs::remove_dir_all(&home);

    assert!(
        text.contains("a named conversation"),
        "the listing does not show the name the run was given: {text:?}"
    );
    assert!(
        text.contains("deleted "),
        "deleting the conversation the run is in is still refused: {text:?}"
    );
    assert!(
        text.contains("started a new session"),
        "the run deleted the conversation it was in and did not say it had started another: {text:?}"
    );
    assert_eq!(
        files.len(),
        1,
        "the run is writing {} conversations after deleting the one it was in: {:?}",
        files.len(),
        files
    );
    assert!(
        body.contains("hello"),
        "the run did not carry on in a fresh conversation: {body:?}"
    );
    assert!(
        !body.contains("a named conversation"),
        "the deleted conversation is the one the run is still writing, so it came back: {body:?}"
    );
}

/// `/archive` on the conversation the run is in: the same rule, because it is the same problem.
///
/// Moving a file out from under the writer hides the conversation being written and has the next
/// event recreate it at the old path -- `/delete`'s failure with a different name. So the run starts a
/// fresh conversation first and files the old one away, and the archived file is the witness that it
/// went into the archive rather than being removed.
#[cfg(debug_assertions)]
#[tokio::test]
async fn archiving_the_open_session_files_it_away_and_starts_a_fresh_one() {
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

    let home = test_home("repl-archive-open", &server.uri());
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
            .write_all(b"/name a named conversation\n/archive 1\nhello\n/exit\n")
            .expect("failed to write stdin");
    }
    let out = child.wait_with_output().expect("flint did not finish");
    let text = String::from_utf8_lossy(&out.stdout).to_string();

    // The archive sits beside the directory the conversation was held in -- a project's is
    // `sessions/<dir>/archive/` -- so it is found rather than assumed to be at the root. The
    // directory may not be there at all: nothing has been archived yet, which is exactly what this
    // test asks about, and a helper that panicked on that would report the shape of the home rather
    // than the behaviour of the command.
    let mut archived: Vec<std::path::PathBuf> = Vec::new();
    for entry in std::fs::read_dir(home.join("sessions")).into_iter().flatten().flatten() {
        let archive = if entry.file_name() == "archive" {
            entry.path()
        } else {
            entry.path().join("archive")
        };
        if archive.is_dir() {
            archived.extend(jsonl_files(&archive));
        }
    }
    archived.sort();
    let filed = archived
        .first()
        .map(|file| std::fs::read_to_string(file).unwrap_or_default())
        .unwrap_or_default();
    let left = jsonl_files(&home.join("sessions"));
    let _ = std::fs::remove_dir_all(&home);

    assert!(
        text.contains("archived "),
        "archiving the conversation the run is in is still refused: {text:?}"
    );
    assert!(
        filed.contains("a named conversation"),
        "the conversation the run was in is not in the archive: {filed:?}"
    );
    assert_eq!(
        left.len(),
        1,
        "the run left {} conversations behind, so the archived one is still being written: {:?}",
        left.len(),
        left
    );
}


/// A warning raised while the REPL is running belongs in the transcript, not on stderr.
///
/// With the strip active a stray write to stderr lands wherever the cursor is -- inside the answer
/// being drawn -- and tears the layout apart. Three sites did exactly that: a session event that
/// could not be persisted (`agent.rs`), a session file with an unreadable line (`session.rs`), and a
/// default config being created under `/reload` (`config.rs`). This drives the one a test can reach
/// deterministically -- `/resume` on a damaged file -- and asserts both halves, because either half
/// alone passes for the wrong reason: a warning that vanished passes "not on stderr", and one
/// printed twice passes a one-sided check.
#[cfg(debug_assertions)]
#[test]
fn a_warning_from_a_resumed_session_is_a_transcript_line_and_not_stderr() {
    let home = test_home("warning-line", "http://127.0.0.1:1/v1");
    let sessions = home.join("sessions");
    std::fs::create_dir_all(&sessions).expect("sessions directory");
    // Damage, not somebody else's event: valid JSON, a type this build knows, and a `message` that
    // is not the object it has to be. That is the distinction `session::load` documents, and it is
    // the one the warning exists for.
    std::fs::write(
        sessions.join("damaged.jsonl"),
        "{\"type\":\"meta\",\"v\":1,\"id\":\"damaged\",\"created\":\"epoch:1\",\"cwd\":\".\",\
         \"provider\":\"stub\",\"model\":\"stub-model\"}\n\
         {\"type\":\"chat\",\"message\":\"not an object\"}\n",
    )
    .expect("damaged session");

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
            .write_all(b"/resume 1\n/exit\n")
            .expect("failed to write stdin");
    }
    let out = child.wait_with_output().expect("flint did not finish");
    let screen = String::from_utf8_lossy(&out.stdout).to_string();
    let errors = String::from_utf8_lossy(&out.stderr).to_string();
    let _ = std::fs::remove_dir_all(&home);

    assert!(
        screen.contains("unreadable line(s) skipped"),
        "the warning never reached the transcript: {screen:?}"
    );
    assert!(
        !errors.contains("unreadable line(s) skipped"),
        "the warning wrote to stderr, which lands inside the strip: {errors:?}"
    );
}

/// The retry ladder says what it is doing in the transcript too.
///
/// The same fault as the test above, one layer down and far more likely to fire: the ladder that
/// waits between attempts printed on stderr from inside the request loop, which is *always* during
/// a turn. A dead endpoint is the cheap way to reach it -- connection refused is retryable, so the
/// ladder walks its 1s, 2s and 4s waits -- and the assertion is the same two-sided one, because a
/// notice that went nowhere and a notice printed twice are different bugs.
///
/// Two things here are shaped by measurement rather than taste. The output goes to *files*, because
/// the ladder is waited for by reading what has been written so far and a pipe nobody is draining
/// would hide exactly that. And `/exit` is written only once the ladder has been seen, because
/// piped input is *steering*: the first version of this test sent `/exit` with the prompt, the turn
/// was dropped before it retried once, and it passed every assertion by never reaching the code
/// under test.
#[cfg(debug_assertions)]
#[test]
fn the_retry_ladder_says_so_in_the_transcript_and_not_on_stderr() {
    let home = test_home("retry-line", "http://127.0.0.1:1/v1");
    let screen_path = home.join("stdout.txt");
    let errors_path = home.join("stderr.txt");

    let mut child = binary()
        .env("FLINT_HOME", &home)
        .env("FLINT_TERM_CAPTURE", "1")
        .env("FLINT_TERM_SIZE", "80x24")
        .env_remove("NO_COLOR")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::from(
            std::fs::File::create(&screen_path).expect("create the screen file"),
        ))
        .stderr(std::process::Stdio::from(
            std::fs::File::create(&errors_path).expect("create the stderr file"),
        ))
        .spawn()
        .expect("failed to run flint");
    {
        use std::io::Write;
        let stdin = child.stdin.as_mut().expect("no stdin handle");
        stdin
            .write_all(b"are you there\n")
            .expect("failed to write stdin");
        stdin.flush().expect("flush");

        // The ladder is four attempts and 1s + 2s + 4s of waiting, so this is generous on a slow
        // machine and short enough that a real hang is reported as one.
        let read = |path: &std::path::Path| std::fs::read_to_string(path).unwrap_or_default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(45);
        while !read(&screen_path).contains("trying in") {
            assert!(
                std::time::Instant::now() < deadline,
                "the ladder never retried; the screen held: {:?}",
                read(&screen_path)
            );
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        stdin.write_all(b"/exit\n").expect("failed to write stdin");
    }
    let _ = child.wait_with_output().expect("flint did not finish");
    let screen = std::fs::read_to_string(&screen_path).unwrap_or_default();
    let errors = std::fs::read_to_string(&errors_path).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&home);

    // "trying in", not "retrying in": the notice is one long line and the strip wraps it, so the
    // captured bytes carry a `\r\n` and a repaint escape in the middle of the word.
    assert!(
        screen.contains("trying in"),
        "the retry left the transcript between the wait and the exit: {screen:?}"
    );
    assert!(
        !errors.contains("trying in"),
        "the retry wrote to stderr, which lands inside the strip: {errors:?}"
    );
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

/// A command one of flint's own tools runs is told which run it is in.
///
/// Driven through the binary rather than the library, because the tool set is filled in inside
/// `Agent::new`: a test that built its own `ToolBox` and called `with_run_env` on it would pass even
/// if that wiring were missing. The command writes what it was told into a file, so the assertion is
/// on the values themselves and not on how a transcript happened to render them, and the session
/// path it reports is compared against the session file the run actually created.
#[tokio::test]
async fn a_command_the_model_runs_is_told_which_run_it_is_in() {
    let server = MockServer::start().await;

    let call = serde_json::json!({
        "choices": [{"delta": {"tool_calls": [{"index": 0, "id": "call_0",
            "function": {"name": "bash",
                         "arguments": serde_json::json!({"command": ask_run("tool-env.txt")}).to_string()}}]}}]
    });
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).insert_header("content-type", "text/event-stream").set_body_string(sse(&[
            &format!("data: {call}"),
            r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
            "data: [DONE]",
        ])))
        .up_to_n_times(1)
        .mount(&server)
        .await;
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

    let home = test_home("run-env-model", &server.uri());
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("work directory");
    let out = binary()
        .args(["-p", "where am I", "--cwd"])
        .arg(&work)
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("failed to run flint");
    assert_eq!(
        out.status.code(),
        Some(0),
        "the run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let session = jsonl_files(&home.join("sessions"))
        .into_iter()
        .next()
        .expect("the run must have written a session file");
    assert_told_which_run(&work.join("tool-env.txt"), &session);
    let _ = std::fs::remove_dir_all(&home);
}

/// The person's own `!cmd`, inside a run, is told the same thing -- one answer for two doors.
///
/// A separate test from the model's, and one that needs no model at all: the escape is a person's
/// command, and the run it belongs to must not describe itself differently depending on who asked.
/// `/name` is typed first because it is what creates the session file, and this asserts against that
/// file's path rather than against a string the test hoped for.
#[cfg(debug_assertions)]
#[tokio::test]
async fn a_command_the_person_types_inside_a_run_is_told_the_same_run() {
    let home = test_home("run-env-person", "http://127.0.0.1:1/v1");
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("work directory");

    let mut child = binary()
        .arg("--cwd")
        .arg(&work)
        .env("FLINT_HOME", &home)
        // The interactive layout, since that is where a person's `!` line lives; the size is the
        // one the other REPL tests use.
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
            .write_all(
                format!(
                    "/name where am I\n!{}\n/exit\n",
                    ask_run("person-env.txt")
                )
                .as_bytes(),
            )
            .expect("failed to write stdin");
    }
    let out = child.wait_with_output().expect("flint did not finish");
    let text = String::from_utf8_lossy(&out.stdout).to_string();

    let session = jsonl_files(&home.join("sessions"))
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("no session file was written: {text:?}"));
    assert_told_which_run(&work.join("person-env.txt"), &session);
    let _ = std::fs::remove_dir_all(&home);
}

/// One command, writing the three variables in the order the assertions read them. No space around
/// `&`, or `cmd`'s `echo` prints one.
fn ask_run(file: &str) -> String {
    if cfg!(windows) {
        format!("(echo %FLINT_SESSION%& echo %FLINT_PROVIDER%& echo %FLINT_MODEL%) > {file}")
    } else {
        format!(
            "printf '%s\\n%s\\n%s\\n' \"$FLINT_SESSION\" \"$FLINT_PROVIDER\" \"$FLINT_MODEL\" > {file}"
        )
    }
}

/// What a command wrote, against what the run says it is.
fn assert_told_which_run(file: &std::path::Path, session: &std::path::Path) {
    let written = std::fs::read_to_string(file)
        .unwrap_or_else(|e| panic!("{} was not written: {e}", file.display()));
    let lines: Vec<&str> = written.lines().map(str::trim).collect();
    assert_eq!(
        lines.first().copied(),
        Some(session.display().to_string().as_str()),
        "{} was not told which conversation it is in: {written:?}",
        file.display()
    );
    assert_eq!(
        lines.get(1).copied(),
        Some("stub"),
        "{} was not told which provider is paying: {written:?}",
        file.display()
    );
    assert_eq!(
        lines.get(2).copied(),
        Some("stub-model"),
        "{} was not told which model is paying: {written:?}",
        file.display()
    );
}

/// `flint exec` is not a conversation, so a command it runs is told *nothing* -- by removal, not by
/// inheritance.
///
/// The variables are planted in flint's own environment, which is exactly the state of a `flint` that
/// was started by a run's command: a model that runs `flint exec ...` through `bash` hands its own
/// variables down. A child told the wrong transcript is worse off than one told none, so the test
/// asserts the planted names do not arrive; without the removal they do, which is what makes this
/// test able to fail.
#[test]
fn exec_does_not_pass_on_a_session_name_it_inherited() {
    let command = if cfg!(windows) {
        "echo [%FLINT_SESSION%] [%FLINT_PROVIDER%]"
    } else {
        "echo \"[$FLINT_SESSION] [$FLINT_PROVIDER]\""
    };
    let out = binary()
        .args(["exec", command])
        .env("FLINT_SESSION", "a-conversation-from-another-run.jsonl")
        .env("FLINT_PROVIDER", "another-run")
        .env_remove("NO_COLOR")
        .output()
        .expect("failed to run flint");
    let text = String::from_utf8_lossy(&out.stdout).to_string();

    assert!(
        !text.contains("a-conversation-from-another-run"),
        "a name flint inherited reached a command it ran: {text:?}"
    );
    assert!(
        !text.contains("another-run"),
        "an endpoint flint inherited reached a command it ran: {text:?}"
    );
}

/// A run told to write no conversation writes none, and tells a program so on its stream.
///
/// Two claims and one run, because they are one promise seen by two readers: a `--json` caller is
/// told the session is `null` rather than handed a path to a file that does not exist, and the
/// directory a person would look in holds nothing. The turn really happens -- the mock answers it and
/// the answer is asserted -- because a run that failed before saying anything would leave no file
/// either, and the test would then pass for the reason it is not about.
#[tokio::test]
async fn a_run_that_writes_no_conversation_writes_none() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse(&[
                    r#"data: {"choices":[{"delta":{"content":"nothing kept"}}]}"#,
                    r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
                    "data: [DONE]",
                ])),
        )
        .mount(&server)
        .await;

    let home = test_home("no-session", &server.uri());
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("work directory");
    let out = binary()
        .args(["-p", "say something", "--json", "--no-session", "--cwd"])
        .arg(&work)
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("failed to run flint");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert_eq!(
        out.status.code(),
        Some(0),
        "the run failed: {text}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        text.contains("nothing kept"),
        "the turn did not happen, so an empty directory would prove nothing: {text}"
    );

    let started = text
        .lines()
        .find(|line| line.contains("\"session.started\""))
        .unwrap_or_else(|| panic!("no session.started frame: {text}"));
    let frame: serde_json::Value = serde_json::from_str(started).expect("the frame is one object");
    assert!(
        frame["session"].is_null(),
        "the stream named a session for a run that was told to write none: {frame}"
    );

    let written = jsonl_files(&home.join("sessions"));
    assert!(
        written.is_empty(),
        "a conversation was written anyway: {written:?}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// A run that may not write a conversation refuses every flag that would open or name one.
///
/// Refused rather than ignored, because both quiet outcomes are bad in the same way: `--no-session
/// --continue` that resumed anyway is a person who believes nothing was written while a file grows,
/// and one that started a fresh conversation anyway is a person who believes they went back. The
/// doors are tried in one test because the claim is "all of them", so a fifth added later lands here.
#[test]
fn a_run_that_writes_no_conversation_refuses_to_open_one() {
    for flag in [
        &["--continue"][..],
        &["--resume"][..],
        &["--resume", "3"][..],
        &["--fork"][..],
        &["--name", "something"][..],
    ] {
        let mut args = vec!["-p", "hello", "--no-session"];
        args.extend_from_slice(flag);
        let (code, out) = run(&args);
        let text = String::from_utf8_lossy(&out);
        assert_eq!(code, 2, "{flag:?} was not refused: {text}");
        assert!(
            text.contains("--no-session") && text.contains(flag[0]),
            "the refusal for {flag:?} does not name what it refuses: {text}"
        );
    }
}

/// The words every refusal in a `--no-session` run has to carry. One sentence for the doors that
/// refuse, so a test can count them rather than guess at three phrasings.
#[cfg(debug_assertions)]
const KEEPS_NO_CONVERSATION: &str = "this run keeps no conversation (--no-session)";

/// The promise holds for the whole run, not only for the command line that started it.
///
/// `/reload` is why this is more than a refusal message, and it is deliberately *not* one of the two
/// that refuse: re-reading the config is a thing a person must be able to do in any run. What it
/// must not do is leave a file, and it would: it rebuilds the agent through `continue_conversation`,
/// which *creates* a session when the old one has no file. So the file count below is what fails if
/// the flag is dropped when the agent is rebuilt, and `/new` typed after `/reload` is the other half
/// of the same fact -- a refusal that is still there after the rebuild, rather than one that was only
/// in the startup path. `/resume` is the third door, and a session is written first so its refusal is
/// about the flag rather than about there being nothing to resume. `/import` is the fourth and it is
/// the one that needs the refusal most: it would *create* a conversation out of a file the run was
/// pointed at, which is the flag's whole promise undone by one command. `/fork` is the fifth, and the
/// same claim from the other side -- it would create a conversation out of *this* run's, which a run
/// that keeps none does not have; the check is first in its arm, so what is being held here is the
/// flag rather than an empty history.
#[cfg(debug_assertions)]
#[tokio::test]
async fn a_run_that_writes_no_conversation_refuses_to_start_one_mid_run() {
    let home = test_home("no-session-repl", "http://127.0.0.1:1/v1");
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("work directory");
    write_session(
        &home.join("sessions"),
        "20260101000000-1-999.jsonl",
        &[&meta_line("20260101000000-1-999")],
        0,
    );
    // A file that *would* import: the refusal has to be about the flag and not about the file.
    let given = home.join("given.jsonl");
    std::fs::write(
        &given,
        format!(
            "{}\n",
            r#"{"type":"chat","message":{"role":"user","content":"a conversation to bring in"}}"#
        ),
    )
    .expect("write the given file");

    let mut child = binary()
        .args(["--no-session", "--cwd"])
        .arg(&work)
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
        child
            .stdin
            .as_mut()
            .expect("no stdin handle")
            .write_all(format!("/reload\n/new\n/resume 1\n/import {}\n/fork 1\n/exit\n", given.display()).as_bytes())
            .expect("failed to write stdin");
    }
    let out = child.wait_with_output().expect("flint did not finish");
    let text = String::from_utf8_lossy(&out.stdout).to_string();

    assert!(
        text.contains("reloaded"),
        "a --no-session run refused to re-read its config, which is not a conversation: {text:?}"
    );
    assert_eq!(
        text.matches(KEEPS_NO_CONVERSATION).count(),
        4,
        "/new, /resume, /import and /fork are the doors that must refuse, and /new refusing after \
         /reload is how the flag surviving a rebuild is shown: {text:?}"
    );
    let written = jsonl_files(&home.join("sessions"));
    assert_eq!(
        written.len(),
        1,
        "the run wrote a conversation it was told not to write: {written:?}"
    );
    let _ = std::fs::remove_dir_all(&home);
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

/// The request is not the transcript, and a long conversation is where that stops being a detail.
///
/// Past `max_request_chars` the oldest turns are left out of the request, and the session file keeps
/// every one of them. The unit tests in `agent.rs` hold the trim itself to its rules; this holds the
/// *run* to them: the config key is read, the trim reaches the request the model would be sent, and
/// nothing was taken out of the file on disk.
#[test]
fn a_long_conversation_is_trimmed_in_the_request_and_not_in_the_session_file() {
    let home = std::env::temp_dir().join(format!("flint-trim-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let sessions = home.join("sessions");
    std::fs::create_dir_all(&sessions).expect("home directory");
    // A budget of two turns and the note: every message below is ~227 characters, so this keeps the
    // last two turns of six along with the question being asked.
    std::fs::write(
        home.join("config.toml"),
        "default_provider = \"stub\"\n\
         max_request_chars = 1200\n\n\
         [[providers]]\n\
         name = \"stub\"\n\
         base_url = \"http://127.0.0.1:1/v1\"\n\
         api_key = \"not-a-real-key\"\n\
         model = \"stub-model\"\n",
    )
    .expect("config file");

    let mut lines = vec![meta_line("trimmed-request")];
    for i in 0..6 {
        lines.push(format!(
            r#"{{"type":"chat","message":{{"role":"user","content":"question {i} {}"}}}}"#,
            "x".repeat(200)
        ));
        lines.push(format!(
            r#"{{"type":"chat","message":{{"role":"assistant","content":"answer {i} {}"}}}}"#,
            "y".repeat(200)
        ));
    }
    let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
    let path = write_session(&sessions, "trimmed-request.jsonl", &refs, 0);

    let out = binary()
        .args(["--resume", "1", "debug", "prompt-input", "the next question"])
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
    let sent = serde_json::to_string(&body["messages"]).expect("messages as text");

    assert!(
        !sent.contains("question 3"),
        "a turn over the budget is still in the request: {sent}"
    );
    assert!(
        !sent.contains("answer 3"),
        "half a dropped turn is still in the request: {sent}"
    );
    assert!(
        sent.contains("question 4") && sent.contains("answer 4"),
        "the trim dropped a turn that fits: {sent}"
    );
    assert!(
        sent.contains("question 5"),
        "the newest turn is what the request is for: {sent}"
    );
    assert!(
        sent.contains("the next question"),
        "the message being asked is not in the request: {sent}"
    );
    assert!(
        sent.contains("8 earlier messages were left out of this request"),
        "the request does not say what was left out: {sent}"
    );

    let on_disk = std::fs::read_to_string(&path).expect("the session file is still there");
    assert!(
        on_disk.contains("question 0") && on_disk.contains("answer 5"),
        "the session file lost a message the request dropped"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// A rename reaches the sidebar it was typed into.
///
/// The rename field lives in the sidebar, and the sidebar shows the name -- so a page that renamed a
/// conversation and went on showing the old label is a rename that looks like it failed, which is the
/// same complaint `/archive` produced ("the page did not refresh") one command over. The frame is the
/// one those two already push, for the same reason: what the list says has changed, and the list is a
/// route (`GET /sessions`) the page re-reads rather than data it is sent.
#[tokio::test]
async fn renaming_a_conversation_tells_the_page_to_read_the_list_again() {
    // No stub server: nothing in this test answers a model, and `/name` appends to the session file.
    let home = test_home("name-frame", "http://127.0.0.1:9/v1");
    std::fs::write(
        home.join("config.toml"),
        "default_provider = \"stub\"\n\n\
         [[providers]]\n\
         name = \"stub\"\n\
         base_url = \"http://127.0.0.1:9/v1\"\n\
         model = \"stub-model\"\n\
         api_key = \"not-a-real-key\"\n",
    )
    .expect("the test config");

    let log = home.join("transcript.txt");
    // The run's stderr, kept rather than sent to `/dev/null`: a `--web` run that dies mid-test
    // otherwise reports a connection that was refused, or a transcript that simply stops, and
    // nothing about *why* -- the class the flaky jobs test came from, which `HANDOFF.md` names.
    // It sits in this test's own home, and a test that panics leaves the whole home behind.
    let errors = home.join("stderr.txt");
    let mut child = binary()
        .arg("--web")
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .stdin(std::process::Stdio::piped())
        .stdout(std::fs::File::create(&log).expect("transcript file"))
        .stderr(std::fs::File::create(&errors).expect("stderr file"))
        .spawn()
        .expect("failed to run flint");

    let (port, token) = port_and_token(&wait_for_url(&log));
    // Connected before the rename, so this is the frame a page that is open right now receives --
    // which is the whole point: the label on screen is the one that is now wrong.
    let mut watching = http_stream(port, "/events", &token);
    let opening = read_until(&mut watching, "\"model\":\"stub-model\"", 20);

    let answered = post_message(port, &token, "/name a better name");
    // A named frame: `event: sessions` with an empty line, because what the page is told is that a
    // *route* (`GET /sessions`) is stale rather than anything about the run.
    let told = read_until(&mut watching, "event: sessions", 20);

    drop(watching);
    drop(child.stdin.take());
    let exited = wait_for_exit(&mut child, 20);
    let transcript = std::fs::read_to_string(&log).unwrap_or_default();
    let written = flint::session::list_detailed(&home.join("sessions"))
        .unwrap_or_default()
        .into_iter()
        .map(|s| s.title.unwrap_or_default())
        .collect::<Vec<_>>();
    let _ = std::fs::remove_dir_all(&home);

    assert!(exited, "flint did not exit");
    assert!(
        opening.contains("\"type\":\"state\""),
        "the page was never told the state, so nothing before the rename was measurable: {opening:?}"
    );
    assert!(
        // 202 and a `queued` body: the route stashes the line for the loop that owns the commands,
        // which is the same answer every other line a page sends gets.
        answered.contains("202 Accepted") && answered.contains("\"queued\":true"),
        "the rename was not accepted: {answered:?} {transcript:?}"
    );
    assert!(
        told.contains("event: sessions"),
        "the rename left the sidebar showing the old name, because nothing told the page the list \
         it draws had changed. Frames: {told:?} Terminal: {transcript:?}"
    );
    assert_eq!(
        written,
        vec!["a better name".to_string()],
        "the title was not written to the conversation that is open"
    );
}

/// The page's half of deleting the conversation the run is *in*: the sidebar is told the list changed,
/// the pane is told to start again, and there is nothing left to list.
///
/// Those are one operation seen by three readers, and the last is the one a person sees: with the old
/// file gone and nothing said in the new conversation, `GET /sessions` has no rows -- which is what
/// puts the sidebar on "no conversations yet" and the right pane back on its default page. That is the
/// state asked for directly on 2026-09-23 ("the right side goes back to the default"), and it is the
/// state DSH keeps for "no conversation chosen": a page, rather than an empty pane. Asserting the
/// frames alone would pass over a route that went on listing the file it had just removed.
#[tokio::test]
async fn deleting_the_open_conversation_tells_the_page_to_start_again_and_lists_nothing() {
    // No stub server: nothing here answers a model, and `/name` and `/delete` are file operations.
    let home = test_home("delete-current-frame", "http://127.0.0.1:9/v1");

    let log = home.join("transcript.txt");
    let errors = home.join("stderr.txt");
    let mut child = binary()
        .arg("--web")
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .stdin(std::process::Stdio::piped())
        .stdout(std::fs::File::create(&log).expect("transcript file"))
        .stderr(std::fs::File::create(&errors).expect("stderr file"))
        .spawn()
        .expect("failed to run flint");

    let (port, token) = port_and_token(&wait_for_url(&log));
    let mut watching = http_stream(port, "/events", &token);
    let opening = read_until(&mut watching, "\"model\":\"stub-model\"", 20);

    // Named first, so there is a file to delete and a label for the listing to be missing afterwards.
    // `/name` pushes a `sessions` frame of its own, and that one is read here rather than counted as
    // the delete's: this test's claim is about the frames the *delete* produces.
    let named = post_message(port, &token, "/name a better name");
    let named_told = read_until(&mut watching, "event: sessions", 20);
    let listed_before = http_get(port, "/sessions", &token);

    let deleted = post_message(port, &token, "/delete 1");
    // A `reset` and not only a `sessions`: the run moved to another conversation, and the pane is
    // rebuilt from the file the run is now writing -- which, with nothing said in it, is the default
    // page. Read to the `reset`, so what comes back carries both frames in the order they are pushed.
    let told = read_until(&mut watching, "event: reset", 20);
    let listed_after = http_get(port, "/sessions", &token);

    drop(watching);
    drop(child.stdin.take());
    let exited = wait_for_exit(&mut child, 20);
    let transcript = std::fs::read_to_string(&log).unwrap_or_default();
    let left = jsonl_files(&home.join("sessions"));
    let _ = std::fs::remove_dir_all(&home);

    assert!(exited, "flint did not exit");
    assert!(
        opening.contains("\"type\":\"state\""),
        "the page was never told the state, so nothing here was measurable: {opening:?}"
    );
    assert!(
        named.contains("202 Accepted") && named_told.contains("event: sessions"),
        "the conversation was not named, so there was nothing to delete: {named:?} {named_told:?}"
    );
    assert!(
        listed_before.contains("a better name"),
        "the named conversation is not in the listing the sidebar draws: {listed_before:?}"
    );
    assert!(
        deleted.contains("202 Accepted"),
        "the delete was not accepted from the page: {deleted:?} {transcript:?}"
    );
    assert!(
        transcript.contains("deleted ") && transcript.contains("started a new session"),
        "the run did not delete the conversation it was in and start a fresh one: {transcript:?}"
    );
    assert!(
        told.contains("event: sessions") && told.contains("event: reset"),
        "the page was not told both halves: the list changed (so the deleted row goes) and the run \
         moved (so the pane is rebuilt on the fresh conversation). Frames: {told:?} \
         Terminal: {transcript:?}"
    );
    assert!(
        listed_after.contains("\"sessions\":[]") && !listed_after.contains("a better name"),
        "the sidebar still has a row for the conversation that was just deleted: {listed_after:?}"
    );
    assert!(
        left.is_empty(),
        "a conversation file is left where the run deleted its own: {left:?}"
    );
}

/// A turn can change the list, so the page is told so -- twice over, and both are things a person
/// watches for.
///
/// The first thing said in a conversation is what *creates* its file, so a run holding a conversation
/// with nothing in it has no row in the sidebar until a turn writes one; and for a conversation nobody
/// has named, the label the sidebar draws is the first question, which the same turn writes. Neither is
/// derivable from the frames a turn already pushes. Found by a real browser on 2026-09-23: the harness
/// deleted the conversation the run was in, asked its first question in the new one, and the sidebar
/// stayed empty while the transcript had both the question and the answer.
///
/// The control is the frame that *is* asserted absent before the message: a `sessions` frame arriving
/// for any other reason would make the claim below pass without the turn having said anything.
#[tokio::test]
async fn saying_something_tells_the_page_the_list_may_have_changed() {
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

    let home = test_home("turn-frame", &server.uri());
    let log = home.join("transcript.txt");
    let errors = home.join("stderr.txt");
    let mut child = binary()
        .arg("--web")
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .stdin(std::process::Stdio::piped())
        .stdout(std::fs::File::create(&log).expect("transcript file"))
        .stderr(std::fs::File::create(&errors).expect("stderr file"))
        .spawn()
        .expect("failed to run flint");

    let (port, token) = port_and_token(&wait_for_url(&log));
    let mut watching = http_stream(port, "/events", &token);
    let opening = read_until(&mut watching, "\"type\":\"state\"", 20);
    let listed_before = http_get(port, "/sessions", &token);

    let asked = post_message(port, &token, "the first thing said");
    let told = read_until(&mut watching, "event: sessions", 20);
    let listed_after = http_get(port, "/sessions", &token);

    drop(watching);
    drop(child.stdin.take());
    let exited = wait_for_exit(&mut child, 20);
    let transcript = std::fs::read_to_string(&log).unwrap_or_default();
    let written = jsonl_files(&home.join("sessions"));
    let body = written
        .first()
        .map(|file| std::fs::read_to_string(file).unwrap_or_default())
        .unwrap_or_default();
    let _ = std::fs::remove_dir_all(&home);

    assert!(exited, "flint did not exit");
    assert!(
        opening.contains("\"type\":\"state\""),
        "the page was never told the state, so nothing here was measurable: {opening:?}"
    );
    assert!(
        !opening.contains("event: sessions"),
        "a `sessions` frame arrived before anything was said, so the claim below could pass without the \
         turn having changed anything. Frames: {opening:?}"
    );
    assert!(
        asked.contains("202 Accepted"),
        "the message was not accepted from the page: {asked:?} {transcript:?}"
    );
    assert!(
        told.contains("event: sessions"),
        "the turn wrote the conversation's first event -- which is what puts a row in the sidebar -- and \
         nothing told the page its list was stale. Frames: {told:?} Terminal: {transcript:?}"
    );
    assert_eq!(
        written.len(),
        1,
        "the turn did not create exactly one conversation file: {written:?}"
    );
    assert!(
        body.contains("the first thing said"),
        "the file the sidebar would label the row from does not hold the question: {body:?}"
    );
    assert!(
        listed_after.contains("the first thing said") && !listed_before.contains("the first thing said"),
        "the route the page re-reads does not show the conversation the turn created: before \
         {listed_before:?} after {listed_after:?}"
    );
}

/// The run's background work, as the page sees it: the frame that says the list changed, the list
/// itself, and the same list again once the job has ended.
///
/// The third one is the point of the whole round. A job nobody is waiting for is exactly the thing a
/// person cannot see from the terminal and cannot ask the model about without interrupting it -- and
/// the fact they are looking for is the exit code, which arrives *after* the frame that announced the
/// job. Driven through the real binary because it is the wiring being tested: a tool call deep in the
/// loop, the record it leaves, the revision counter, and a listener in another task.
#[tokio::test]
async fn a_background_command_is_a_job_the_page_can_watch_end() {
    // A command that takes a few seconds, in the words the platform's own shell understands -- the
    // same shape `tools::tests` uses for the same reason: long enough to be seen running, short
    // enough that no test waits on it.
    let command = if cfg!(windows) {
        "echo one & ping -n 4 127.0.0.1 >NUL & echo two"
    } else {
        "echo one; sleep 3; echo two"
    };

    let server = MockServer::start().await;
    // The first turn asks for the command in the background. Built with `serde_json` rather than
    // written by hand: the command line carries quotes, `&` and `>`, and an escaping mistake here
    // would be a test that quietly asks for something else.
    let arguments = serde_json::json!({ "command": command, "background": true }).to_string();
    let call = serde_json::json!({
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0, "id": "call_0",
                    "function": { "name": "bash", "arguments": arguments },
                }],
            },
        }],
    });
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse(&[
                    &format!("data: {call}"),
                    r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
                    "data: [DONE]",
                ])),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    // Then the model is asked again, with the handle in front of it, and answers.
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).insert_header("content-type", "text/event-stream").set_body_string(sse(&[
            r#"data: {"choices":[{"delta":{"content":"started it"}}]}"#,
            r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
            "data: [DONE]",
        ])))
        .mount(&server)
        .await;

    let home = test_home("jobs-frame", &server.uri());
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("the working directory");
    let log = home.join("transcript.txt");
    // stderr to a file rather than `/dev/null`: this test refused a connection on the ubuntu runner
    // once, and the reason it could not say why was here. See `get_or_say`.
    let errors = home.join("stderr.txt");
    let mut child = binary()
        .arg("--web")
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .current_dir(&work)
        .stdin(std::process::Stdio::piped())
        .stdout(std::fs::File::create(&log).expect("transcript file"))
        .stderr(std::fs::File::create(&errors).expect("stderr file"))
        .spawn()
        .expect("failed to run flint");

    let (port, token) = port_and_token(&wait_for_url(&log));
    // Connected before the job starts, so the frame below is one a page that is open right now
    // receives rather than one it would have to re-read for.
    let mut watching = http_stream(port, "/events", &token);
    let opening = read_until(&mut watching, "\"model\":\"stub-model\"", 20);

    let answered = post_message(port, &token, "start it");
    let started = read_until(&mut watching, "event: jobs", 30);
    let listed = get_or_say(port, "/jobs", &token, &mut child, &errors, &log);
    let running: serde_json::Value =
        serde_json::from_str(&listed).unwrap_or_else(|e| panic!("not JSON ({e}): {listed:?}"));
    let row = running["jobs"]
        .as_array()
        .expect("a list of jobs")
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("the run started a job and the page cannot see it: {listed}"));
    let path = row["path"].as_str().unwrap_or_default().to_string();

    // Wait for it to end, the way a page does: by re-reading the route. The exit code is what the
    // list is for, and it only exists after the command does.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut settled: serde_json::Value = row.clone();
    while std::time::Instant::now() < deadline {
        let again = get_or_say(port, "/jobs", &token, &mut child, &errors, &log);
        settled = serde_json::from_str::<serde_json::Value>(&again)
            .unwrap_or_else(|e| panic!("not JSON ({e}): {again:?}"))["jobs"]
            .as_array()
            .and_then(|rows| rows.first().cloned())
            .unwrap_or_else(|| panic!("the job disappeared from the list: {again}"));
        if settled["status"] != "running" {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    // The second frame, and it is a second one: the page was told the list had changed when the job
    // started and is told again when it ends, which is the difference between a list that is right
    // and a list that is right for a moment.
    let ended = read_until(&mut watching, "event: jobs", 30);

    drop(watching);
    drop(child.stdin.take());
    let exited = wait_for_exit(&mut child, 20);
    let output = std::fs::read_to_string(&path).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&home);

    assert!(exited, "flint did not exit");
    assert!(
        opening.contains("\"type\":\"state\""),
        "nothing before the job was measurable: {opening:?}"
    );
    assert!(
        answered.contains("202 Accepted"),
        "the line never reached the run: {answered:?}"
    );
    assert!(
        started.contains("event: jobs"),
        "the job was invisible to the page: {started:?}"
    );
    assert_eq!(row["kind"], "command", "the kind a command's row carries: {row}");
    assert_eq!(row["status"], "running", "it was listed while it ran: {row}");
    assert_eq!(
        row["label"], command,
        "the row has to say what was asked, or two running jobs look alike: {row}"
    );
    // The path is checked once the command has ended, not here: the shell creates its own redirect
    // a moment after the spawn, so a file that does not exist yet is not a defect -- and asserting on
    // the *output* below is the claim that matters.
    assert!(
        path.ends_with("background-bash-1.log"),
        "the row must point at the command's own log: {row}"
    );
    assert_eq!(
        settled["status"], "completed",
        "the command ended and the page still says otherwise: {settled}"
    );
    assert!(
        settled["detail"].as_str().unwrap_or_default().contains("exit code 0"),
        "the list must carry the fact a person is looking for: {settled}"
    );
    assert!(
        ended.contains("event: jobs"),
        "the end of the job was never announced, so the page would show it running until something \
         else happened: {ended:?}"
    );
    assert!(
        output.contains("one") && output.contains("two"),
        "the log the row points at is not the command's output: {output:?}"
    );
    assert!(path.contains("spill"), "and it is where this run keeps its files: {path:?}");
}

/// A person can read the run's jobs and end one -- and the page reaches both through its own rows.
///
/// The panel is a *reading*: DSH's jobs panel has no kill control either, and until this the only
/// thing that could end a job was `job_op`, which is a tool and therefore the model's. So a person
/// watching a build they no longer wanted had nothing to type: `/jobs` lists what this run started
/// and `/jobs stop <pid>` ends one, and the page is handed both the way it is handed every other
/// command -- the listing as a report (`POST /report`, the `panel` class) and the stop as a
/// destructive row whose candidates are the pids the jobs panel is already showing.
///
/// The command is deliberately longer than any test would wait: what is being measured is that the
/// stop arrives, not that the job could have ended on its own.
#[tokio::test]
async fn a_person_can_read_the_run_s_jobs_and_stop_one() {
    let command = if cfg!(windows) {
        "ping -n 30 127.0.0.1 >NUL & echo never"
    } else {
        "sleep 30; echo never"
    };

    let server = MockServer::start().await;
    let arguments = serde_json::json!({ "command": command, "background": true }).to_string();
    let call = serde_json::json!({
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0, "id": "call_0",
                    "function": { "name": "bash", "arguments": arguments },
                }],
            },
        }],
    });
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse(&[
                    &format!("data: {call}"),
                    r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
                    "data: [DONE]",
                ])),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).insert_header("content-type", "text/event-stream").set_body_string(sse(&[
            r#"data: {"choices":[{"delta":{"content":"started it"}}]}"#,
            r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
            "data: [DONE]",
        ])))
        .mount(&server)
        .await;

    let home = test_home("jobs-stop", &server.uri());
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("the working directory");
    let log = home.join("transcript.txt");
    // The run's stderr, kept rather than sent to `/dev/null`: a `--web` run that dies mid-test
    // otherwise reports a connection that was refused, or a transcript that simply stops, and
    // nothing about *why* -- the class the flaky jobs test came from, which `HANDOFF.md` names.
    // It sits in this test's own home, and a test that panics leaves the whole home behind.
    let errors = home.join("stderr.txt");
    let mut child = binary()
        .arg("--web")
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .current_dir(&work)
        .stdin(std::process::Stdio::piped())
        .stdout(std::fs::File::create(&log).expect("transcript file"))
        .stderr(std::fs::File::create(&errors).expect("stderr file"))
        .spawn()
        .expect("failed to run flint");

    let (port, token) = port_and_token(&wait_for_url(&log));
    let mut watching = http_stream(port, "/events", &token);
    // The frame the page is handed before anything happens: the two rows a person stops a job with
    // have to be in it, because they are drawn from it.
    let opening = read_until(&mut watching, "\"type\":\"state\"", 20);

    post_message(port, &token, "start it");
    let _started = read_until(&mut watching, "event: jobs", 30);
    // The pid is the page's to hold: its rows carry it, which is what makes it a candidate for the
    // destructive row below rather than something the page has to parse out of a sentence.
    let listed = get_or_say(port, "/jobs", &token, &mut child, &errors, &log);
    let row: serde_json::Value = serde_json::from_str::<serde_json::Value>(&listed)
        .unwrap_or_else(|e| panic!("not JSON ({e}): {listed:?}"))["jobs"]
        .as_array()
        .and_then(|rows| rows.first().cloned())
        .unwrap_or_else(|| panic!("the run started a job and the page cannot see it: {listed}"));
    let pid = row["pid"].as_u64().expect("a pid");

    // `/jobs` as the page asks for it: a *report*, which is the class the row carries in the frame
    // above. The answer arrives on the same stream as a turn's events, in the panel it opened.
    let asked = post_to(port, &token, "/report", "/jobs");
    let report = read_until(&mut watching, "\"panel\":true", 20);
    // And the stop, as the page's second press sends it: the line the destructive row composes.
    let stopped = post_message(port, &token, &format!("/jobs stop {pid}"));
    let _ended = read_until(&mut watching, "event: jobs", 30);

    // The kill has to land, and the list is where a person sees that it did.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut settled: serde_json::Value = row.clone();
    while std::time::Instant::now() < deadline {
        let again = get_or_say(port, "/jobs", &token, &mut child, &errors, &log);
        settled = serde_json::from_str::<serde_json::Value>(&again)
            .unwrap_or_else(|e| panic!("not JSON ({e}): {again:?}"))["jobs"]
            .as_array()
            .and_then(|rows| rows.first().cloned())
            .unwrap_or_else(|| panic!("the job disappeared from the list: {again}"));
        if settled["status"] != "running" {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }

    drop(watching);
    drop(child.stdin.take());
    let exited = wait_for_exit(&mut child, 30);
    let _ = std::fs::remove_dir_all(&home);

    assert!(exited, "flint did not exit");
    // Only the command list, so a failure here is readable: the whole `state` frame is one long line
    // and the interesting part of it is a third of the way in.
    let commands = opening
        .split("\"commands\":[")
        .nth(1)
        .and_then(|rest| rest.split("],\"model\"").next())
        .unwrap_or_default();
    assert!(
        commands.contains("\"send\":\"/jobs\"") && commands.contains("\"class\":\"panel\""),
        "the listing is not a row the page can ask for: {commands:?}"
    );
    assert!(
        commands.contains("\"send\":\"/jobs stop\"") && commands.contains("\"from\":\"jobs\""),
        "the page has no stop to offer, or does not know where its candidates are: {commands:?}"
    );
    assert!(
        asked.contains("202 Accepted"),
        "the report never reached the run: {asked:?}"
    );
    assert!(
        report.contains(&format!("pid {pid}")) && report.contains("running for"),
        "the listing a person asked for does not say what is running: {report:?}"
    );
    assert!(
        stopped.contains("202 Accepted"),
        "the stop line never reached the run: {stopped:?}"
    );
    assert_eq!(
        settled["status"], "killed",
        "the job was asked to stop and the list does not say so: {settled}"
    );
    assert!(
        settled["detail"].as_str().unwrap_or_default().contains("exit code -1"),
        "a kill and a failure have to read differently, and this is where: {settled}"
    );
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

/// A fork copies the conversation and then writes somewhere else, and the original is not touched.
///
/// That is the whole promise, and it is a promise about *bytes*: `--fork` exists because `cp`
/// already does this and knowing where flint keeps a session -- or that a conversation is one
/// file at all -- should not be the price of branching one. The failure this pins is the obvious
/// implementation of a fork: load the conversation and then continue in the file it was loaded
/// from, which is `--resume` with a friendlier name and quietly appends the branch to the
/// original.
#[tokio::test]
async fn a_forked_session_is_a_copy_and_the_original_is_untouched() {
    let server = MockServer::start().await;
    answer_once(&server).await;
    let home = test_home("fork", &server.uri());
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("working directory");
    let original = write_session(
        &home.join("sessions"),
        "111-1.jsonl",
        &[
            &meta_line("111-1"),
            r#"{"type":"chat","message":{"role":"user","content":"the question worth branching"}}"#,
            r#"{"type":"chat","message":{"role":"assistant","content":"the answer it got"}}"#,
            r#"{"type":"title","name":"branchy"}"#,
        ],
        10,
    );
    let before = std::fs::read(&original).expect("read the original");

    let out = binary()
        .current_dir(&work)
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .args(["--fork", "111-1", "-p", "now branch it"])
        .output()
        .expect("failed to run flint");
    assert!(
        out.status.success(),
        "flint --fork failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert_eq!(
        std::fs::read(&original).expect("read the original again"),
        before,
        "the original session was written to: a fork is a copy, not a resume"
    );

    let copies: Vec<std::path::PathBuf> = jsonl_files(&home.join("sessions"))
        .into_iter()
        .filter(|path| *path != original)
        .collect();
    assert_eq!(
        copies.len(),
        1,
        "a fork must leave exactly one new session behind: {copies:?}"
    );
    let copied = std::fs::read_to_string(&copies[0]).expect("read the copy");
    for needed in [
        "the question worth branching",
        "the answer it got",
        "now branch it",
        r#""name":"branchy""#,
    ] {
        assert!(
            copied.contains(needed),
            "the copy does not hold `{needed}`: {copied}"
        );
    }

    // Both names, because the question a fork raises five minutes later is which file is which.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("forking 111-1.jsonl"),
        "the run did not say what it was copying: {stderr}"
    );
    assert!(
        stderr.contains("forked into") && stderr.contains("the original is untouched"),
        "the run did not say where the copy went: {stderr}"
    );
}

/// `--fork` and `--resume` answer the same question -- which file does this run write -- and
/// picking one of two answers is how an afternoon's work ends up somewhere unexpected. Refused,
/// with both names in the message, rather than won by whichever the code checks first.
#[test]
fn forking_and_resuming_at_once_is_refused() {
    let home = test_home("fork-conflict", "http://127.0.0.1:1/v1");
    let out = binary()
        .env("FLINT_HOME", &home)
        .args(["--fork", "111-1", "--resume", "111-1", "-p", "x"])
        .output()
        .expect("failed to run flint");
    assert!(!out.status.success(), "the two flags were accepted together");
    let stderr = String::from_utf8_lossy(&out.stderr);
    for needed in ["--fork", "--resume"] {
        assert!(
            stderr.contains(needed),
            "the refusal must name `{needed}`: {stderr}"
        );
    }
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

/// A conversation you switch to is drawn, and not only loaded.
///
/// `/resume` moved the run to another file and said one line about it -- the name and the count of
/// messages -- so the screen after a switch held the line and nothing else. It is the same fault the
/// startup path fixed: a conversation that was loaded and not drawn cannot be told apart from an
/// empty one, and seeing where the conversation got to is the whole reason for going back to it. The
/// page has always drawn it -- `Viewer::follow` makes a browser re-read the file the run moved to --
/// and the terminal was the door that showed nothing.
#[tokio::test]
async fn a_resumed_conversation_is_drawn_and_not_only_loaded() {
    let server = MockServer::start().await;
    answer_once(&server).await;
    let (stdout, _sent) = resume_and_capture(
        &server,
        "resume-draws",
        &[
            &meta_line("111-1"),
            r#"{"type":"chat","message":{"role":"user","content":"the socket question from yesterday"}}"#,
            r#"{"type":"chat","message":{"role":"assistant","content":"the socket answer from yesterday"}}"#,
        ],
    )
    .await;

    assert!(
        stdout.contains("the socket question from yesterday"),
        "/resume moved to a conversation and drew none of it: {stdout:?}"
    );
    assert!(
        stdout.contains("the socket answer from yesterday"),
        "/resume drew what the person asked and not what was answered: {stdout:?}"
    );
}

/// A stub home whose provider says where a reasoning level goes.
///
/// `test_home`'s config with one line added, because the two facts these tests are about are only
/// visible in a *request*: `response_format` for the answer shape, and the level in whatever field the
/// endpoint names. With no `thinking_field` flint sends no reasoning at all whatever the level is, so
/// a test that means to observe a level has to give the endpoint one.
fn home_with_a_thinking_field(tag: &str, base_url: &str) -> std::path::PathBuf {
    let home = test_home(tag, base_url);
    std::fs::write(
        home.join("config.toml"),
        format!(
            "default_provider = \"stub\"\n\n\
             [[providers]]\n\
             name = \"stub\"\n\
             base_url = \"{base_url}\"\n\
             model = \"stub-model\"\n\
             api_key = \"not-a-real-key\"\n\
             thinking_field = \"reasoning_effort\"\n"
        ),
    )
    .expect("the test config");
    home
}

/// Run the REPL in `home`, type `input` at it, and hand back what it printed and the first request
/// body it sent.
///
/// One command and then one question is the shape this is for, and the order matters: a command that
/// arrives while a turn is in flight is run as the command it is, so a longer input would make the
/// request order a matter of timing rather than of the input.
async fn one_command_then_one_question(
    server: &MockServer,
    home: &std::path::Path,
    input: &str,
) -> (String, serde_json::Value) {
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("working directory");
    let mut child = binary()
        .current_dir(&work)
        .env("FLINT_HOME", home)
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
            .write_all(input.as_bytes())
            .expect("failed to write stdin");
    }
    let out = child.wait_with_output().expect("flint did not finish");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let requests = server.received_requests().await.expect("requests");
    let body: serde_json::Value = requests
        .first()
        .map(|r| serde_json::from_slice(&r.body).expect("the request body is JSON"))
        .expect("no request was made, so the turn never ran");
    (stdout, body)
}

/// A conversation switched to inside a run brings its own reasoning level and answer shape.
///
/// The rule at startup is that the conversation's own file has the last word: `resolve_thinking` and
/// `resolve_output_schema` read the last `thinking` and `schema` lines, and the run holds at what they
/// say. `/resume` is the same switch by another door, and it kept the level and the shape of the
/// conversation being *left* -- so a person who had chosen `high` in the one they were going back to
/// watched the run ask for whatever the other one was at, with nothing saying so. Measured
/// 2026-09-23, while hunting the report that the page's settings screen opens on values that are not
/// the ones in force: this is a second way for the level to disagree with the file that chose it.
#[tokio::test]
async fn a_resumed_conversation_brings_its_own_level_and_shape() {
    let server = MockServer::start().await;
    answer_once(&server).await;
    let home = home_with_a_thinking_field("resume-keeps-its-own", &server.uri());
    let shape = r#"{"type":"object","properties":{"day":{"type":"string"}},"required":["day"]}"#;
    write_session(
        &home.join("sessions"),
        "111-1.jsonl",
        &[
            &meta_line("111-1"),
            r#"{"type":"thinking","level":"high"}"#,
            &format!(r#"{{"type":"schema","schema":{shape}}}"#),
            r#"{"type":"chat","message":{"role":"user","content":"the socket question"}}"#,
            r#"{"type":"chat","message":{"role":"assistant","content":"the socket answer"}}"#,
        ],
        10,
    );

    let (stdout, body) = one_command_then_one_question(
        &server,
        &home,
        "/resume 111-1\nand what about the write end\n",
    )
    .await;
    let _ = std::fs::remove_dir_all(&home);

    assert!(
        stdout.contains("the socket question"),
        "the switch drew nothing, so the level below is about a conversation nobody resumed: \
         {stdout:?}"
    );
    assert_eq!(
        body["reasoning_effort"], "high",
        "the resumed conversation's own level was not the one in force, so the run asks for \
         something the person did not choose: {body}"
    );
    assert!(
        body["response_format"].is_object(),
        "the resumed conversation's own answer shape was dropped, so a caller promised JSON is sent \
         prose: {body}"
    );
}

/// A branch starts where the conversation it was cut from is, level and shape included.
///
/// `/fork` exists so that one question can be asked again, differently. A branch is *this*
/// conversation cut short -- its own file has no `thinking` line and no `schema` line, because nobody
/// decided either in it -- so a rebuild that does not hand the two over silently drops what the person
/// asked for, which is the same fault `/model` and `/resume` had. Measured 2026-09-23.
///
/// The cut is at the *second* question on purpose: `/fork 1` on a conversation holding one question is
/// refused ("nothing to keep"), and a refused fork leaves the run where it was -- which is a way for
/// this test to pass while testing nothing, which is what it did before the `forked:` assertion below
/// was added.
#[tokio::test]
async fn a_forked_branch_is_held_at_what_the_run_is_at() {
    let server = MockServer::start().await;
    answer_once(&server).await;
    let home = home_with_a_thinking_field("fork-keeps-its-own", &server.uri());
    let shape = r#"{"type":"object","properties":{"day":{"type":"string"}},"required":["day"]}"#;
    write_session(
        &home.join("sessions"),
        "111-1.jsonl",
        &[
            &meta_line("111-1"),
            r#"{"type":"thinking","level":"high"}"#,
            &format!(r#"{{"type":"schema","schema":{shape}}}"#),
            r#"{"type":"chat","message":{"role":"user","content":"the socket question"}}"#,
            r#"{"type":"chat","message":{"role":"assistant","content":"the socket answer"}}"#,
            r#"{"type":"chat","message":{"role":"user","content":"and the write end"}}"#,
            r#"{"type":"chat","message":{"role":"assistant","content":"it closes too"}}"#,
        ],
        10,
    );

    let (stdout, body) =
        one_command_then_one_question(&server, &home, "/resume 111-1\n/fork 2\nask it again\n").await;
    let _ = std::fs::remove_dir_all(&home);

    // Checked first, because a `/fork` that was refused leaves the run in the conversation it was
    // already in -- where the level is right for a reason that has nothing to do with this door, and
    // the test would pass while testing nothing.
    assert!(
        stdout.contains("forked:"),
        "the branch was never made, so the two facts below are about the conversation `/fork` was \
         supposed to leave: {stdout:?}"
    );
    // `high` is also the proof that `/resume` handed the level over -- the branch was cut from the
    // conversation it made -- and the shape is the half a branch can lose with nothing on screen to
    // say so, which is why both are asserted rather than the one the fork is more obviously about.
    assert_eq!(
        body["reasoning_effort"], "high",
        "the branch was not held at the level the conversation it was cut from was at: {body}"
    );
    assert!(
        body["response_format"].is_object(),
        "the branch lost the answer shape its conversation was being held to: {body}"
    );
}

/// An imported conversation brings its own level and shape, as a resumed one does.
///
/// `/import` and `/resume` differ in who owns the file afterwards, not in what the file says about the
/// run: an imported conversation is the one the person is now in, so its last `thinking` and `schema`
/// lines are what this run asks for. The file here is `low` with a shape of its own, which is what
/// makes the assertion about the import rather than about what was in force before it.
#[tokio::test]
async fn an_imported_conversation_brings_its_own_level_and_shape() {
    let server = MockServer::start().await;
    answer_once(&server).await;
    let home = home_with_a_thinking_field("import-keeps-its-own", &server.uri());
    let shape = r#"{"type":"object","properties":{"why":{"type":"string"}},"required":["why"]}"#;
    let given = home.join("given-to-me.jsonl");
    let schema_line = format!(r#"{{"type":"schema","schema":{shape}}}"#);
    std::fs::write(
        &given,
        format!(
            "{}\n{}\n{}\n{}\n",
            r#"{"type":"thinking","level":"low"}"#,
            schema_line,
            r#"{"type":"chat","message":{"role":"user","content":"why does the socket close early"}}"#,
            r#"{"type":"chat","message":{"role":"assistant","content":"because the peer went away"}}"#,
        ),
    )
    .expect("the given file");

    let (_stdout, body) = one_command_then_one_question(
        &server,
        &home,
        &format!("/import {}\nand what about the write end\n", given.display()),
    )
    .await;
    let _ = std::fs::remove_dir_all(&home);

    assert_eq!(
        body["reasoning_effort"], "low",
        "the imported conversation's own level was not the one in force: {body}"
    );
    assert!(
        body["response_format"].is_object(),
        "the imported conversation's answer shape was dropped: {body}"
    );
}

/// A run-level decision before the first word leaves no session behind, door by door.
///
/// Reported directly on 2026-09-23 -- "a bunch of empty sessions" -- and the files in that real home
/// were `meta` plus exactly one line: a `thinking` from a run told `--thinking high` (or a person
/// typing `/thinking high` and then closing the window), or a `switch` from a `/model` or a `/reload`
/// before anything was said. Each one is a conversation in `/sessions`, in the page's sidebar and in
/// `--continue` -- with no messages in it, which is also what `--continue` resumes when it is the
/// newest.
///
/// `SessionWriter`'s own tests hold the mechanism (the level is held, the switch retargets the pending
/// `meta`); this one holds the doors, because a mechanism nothing comes through is worth nothing. The
/// last phase is the other half of the same claim: what a run *does* decide is still kept, and is in the
/// file under `meta` when the conversation finally begins.
#[test]
fn a_run_level_decision_before_the_first_word_leaves_no_session() {
    let home = test_home("no-empty-session", "http://127.0.0.1:1/v1");
    std::fs::write(
        home.join("config.toml"),
        "default_provider = \"stub\"\n\n\
         [[providers]]\n\
         name = \"stub\"\n\
         base_url = \"http://127.0.0.1:1/v1\"\n\
         model = \"stub-model\"\n\
         models = [\"stub-other\"]\n\
         api_key = \"not-a-real-key\"\n",
    )
    .expect("the test config");
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("working directory");
    let conversations = || -> Vec<String> {
        let mut found: Vec<String> = Vec::new();
        let sessions = home.join("sessions");
        let mut dirs = vec![sessions.clone()];
        while let Some(dir) = dirs.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    dirs.push(path);
                } else if path.extension().is_some_and(|e| e == "jsonl") {
                    found.push(
                        std::fs::read_to_string(&path)
                            .unwrap_or_else(|_| "<unreadable>".to_string()),
                    );
                }
            }
        }
        found
    };

    let doors: [(&str, &[&str], &[&str]); 5] = [
        ("--thinking high", &["--thinking", "high"], &[]),
        ("--no-schema", &["--no-schema"], &[]),
        ("/thinking high", &[], &["/thinking high"]),
        ("/reload", &[], &["/reload"]),
        ("/model stub-other", &[], &["/model stub-other"]),
    ];
    for (name, args, lines) in doors {
        repl_of_with(&home, &work, args, lines);
        assert!(
            conversations().is_empty(),
            "{name} on a run that had said nothing left a session file behind, and it is a \
             conversation every listing will show: {:?}",
            conversations()
        );
    }

    // ...and the same run, once it says something, keeps what it decided: the file exists, begins with
    // the model it is actually on, and carries no `switch` line -- nothing switched, the conversation
    // simply started there.
    let out = repl_of_with(
        &home,
        &work,
        &["--thinking", "high"],
        &["/model stub-other", "hello"],
    );
    let found = conversations();
    assert_eq!(
        found.len(),
        1,
        "a conversation that was spoken in did not leave exactly one session: {found:?} / {out:?}"
    );
    let text = &found[0];
    assert!(
        text.contains(r#""model":"stub-other""#),
        "the conversation began under the model it was switched to and the file does not say so: {text}"
    );
    assert!(
        !text.contains(r#""type":"switch""#),
        "a conversation that had not started recorded a switch as though one had happened: {text}"
    );
    assert!(
        text.contains(r#""type":"thinking""#) && text.contains(r#""level":"high""#),
        "the level this run was told is not in the conversation it went on to have: {text}"
    );
    assert!(
        text.contains(r#""role":"user","content":"hello""#),
        "the conversation does not hold what was said in it: {text}"
    );
}

/// A conversation somebody handed you becomes one of yours, and their file is left alone.
///
/// `/import` is `--fork`'s act for a file this run did not start from: the conversation is *copied*
/// into this run's own sessions and the source is not written to at all. That is the difference from
/// `--resume <path>`, where the run carries on inside the file it was handed -- which is the wrong
/// thing to do to a file somebody gave you, and the reason the door exists rather than a note in the
/// documentation telling people to `cp` first. The fixture is the *hand-written* case twice over: it
/// has no `meta` line at all, and the file lives outside the sessions directory where no listing
/// would ever find it.
#[test]
fn an_imported_conversation_is_copied_in_and_its_source_is_left_alone() {
    let home = test_home("import-copies", "http://127.0.0.1:1/v1");
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("working directory");
    let given = home.join("given-to-me.jsonl");
    let lines = [
        r#"{"type":"chat","message":{"role":"user","content":"why does the socket close early"}}"#,
    ];
    std::fs::write(&given, format!("{}\n", lines.join("\n"))).expect("write the given file");
    let before = std::fs::read(&given).expect("read the given file");

    let text = repl_of(&home, &work, &[&format!("/import {}", given.display()), "/exit"]);

    assert!(
        text.contains("(1 message)"),
        "a one-message import was reported as \"1 messages\": {text:?}"
    );
    assert!(
        text.contains("given-to-me.jsonl"),
        "/import did not name the file the conversation came from: {text:?}"
    );
    assert!(
        text.contains("why does the socket close early"),
        "/import copied the conversation and drew none of it: {text:?}"
    );
    assert_eq!(
        std::fs::read(&given).expect("read the given file again"),
        before,
        "/import wrote to the file it was handed; that is what --resume is for"
    );

    // The copy is a conversation of this run's own: in this home's sessions, with the messages in
    // it and a line saying where they came from, so the provenance survives the import.
    let written = jsonl_files(&home.join("sessions"));
    assert_eq!(
        written.len(),
        1,
        "/import did not leave exactly one conversation of its own: {written:?}"
    );
    let copy = std::fs::read_to_string(&written[0]).expect("read the copy");
    assert!(
        copy.contains("why does the socket close early"),
        "the copy does not hold the imported conversation: {copy:?}"
    );
    assert!(
        copy.contains(r#""type":"import""#) && copy.contains("given-to-me.jsonl"),
        "the copy does not say where it came from: {copy:?}"
    );
    // ...above the conversation it describes, so a person reading the file top to bottom is told
    // where it came from before being told what was said in it.
    let lines: Vec<&str> = copy.lines().collect();
    let provenance = lines
        .iter()
        .position(|l| l.contains(r#""type":"import""#))
        .expect("the import line");
    let conversation = lines
        .iter()
        .position(|l| l.contains(r#""type":"chat""#))
        .expect("the first imported message");
    assert!(
        provenance < conversation,
        "the copy records where it came from below the conversation: {copy:?}"
    );
    assert!(
        copy.contains(r#""type":"meta""#),
        "the copy is a conversation with no meta line: {copy:?}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// `--version` answers the first question a bug report asks, and it answers with the banner's number.
///
/// `ROADMAP.md` §11 item 5 measured the state this replaced: `flint --version` was an unknown flag, and
/// the build's number appeared in exactly two places, neither of them a caller's -- the interactive
/// banner and the `User-Agent` flint sends when it fetches a URL. So flint told the *network* which
/// build it was and not the program that started it, while the `--json` stream carried a field called
/// `version` that is the session file format's. The flag is one line and needs no config, no key and no
/// terminal, which is the point: a build script or an installer asking which flint it is must not start
/// a run to find out.
#[test]
fn the_version_flag_prints_the_build_the_banner_prints() {
    let out = binary()
        .arg("--version")
        .output()
        .expect("failed to run flint --version");
    assert!(
        out.status.success(),
        "flint --version failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let said = String::from_utf8_lossy(&out.stdout);
    let version = said.trim().to_string();
    assert!(
        version.starts_with("flint ") && version.len() > "flint ".len(),
        "the flag does not print one line naming the build: {said:?}"
    );

    // The same number the banner shows, because two numbers for one build is how a bug report starts
    // with the wrong one. The banner is read off a real run, which is where a person meets it.
    let home = test_home("version-flag", "http://127.0.0.1:1/v1");
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("working directory");
    let banner = repl_of(&home, &work, &["/exit"]);
    let from_banner = banner
        .split_whitespace()
        .find(|word| {
            word.strip_prefix('v')
                .is_some_and(|rest| !rest.is_empty() && rest.starts_with(|c: char| c.is_ascii_digit()))
        })
        .unwrap_or_else(|| panic!("the banner carries no version: {banner:?}"));
    assert_eq!(
        version,
        format!("flint {}", &from_banner[1..]),
        "the flag and the banner disagree: {banner:?}"
    );

    let _ = std::fs::remove_dir_all(&home);
}

/// `/export` writes the page `flint export` writes, for the conversation this run is holding.
///
/// The artifact is the one thing that does not change between the two doors, and that is the point:
/// the page is built by one function out of the session's own lines, so a page written in the middle
/// of a conversation and a page written after it are the same kind of file. What the door adds is the
/// answer the CLI does not have to give -- where the page goes when stdout is a terminal -- and it is
/// the person's word rather than a guess: the test writes to a path it named, and the line the run
/// prints is the CLI's own sentence, so the two doors read the same afterwards.
#[test]
fn export_writes_this_conversations_page_where_it_is_told() {
    let home = test_home("export-here", "http://127.0.0.1:1/v1");
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("working directory");
    let given = home.join("given.jsonl");
    let lines = [
        r#"{"type":"chat","message":{"role":"user","content":"why does the socket close early"}}"#,
        r#"{"type":"chat","message":{"role":"assistant","content":"because the peer half-closed it"}}"#,
    ];
    std::fs::write(&given, format!("{}\n", lines.join("\n"))).expect("write the given file");
    let page = work.join("page.html");

    let text = repl_of(
        &home,
        &work,
        &[
            &format!("/import {}", given.display()),
            "/help",
            &format!("/export {}", page.display()),
            "/exit",
        ],
    );

    assert!(
        text.contains("/export <file>"),
        "`/help` does not offer the command: {text:?}"
    );
    assert!(
        text.contains(&format!("wrote {} (", page.display())),
        "/export did not say where the page went: {text:?}"
    );

    let html = std::fs::read_to_string(&page).expect("the exported page");
    // The island `web::export_html` hangs a conversation on, with the conversation in it: the page is
    // the renderer the browser view uses, not a second one written for the file.
    assert!(
        html.contains(r#"<script id="session" type="application/json">"#),
        "the exported file is not a page with a conversation island in it: {} bytes",
        html.len()
    );
    assert!(
        html.contains("why does the socket close early")
            && html.contains("because the peer half-closed it"),
        "the page does not hold the conversation: {html:.400}"
    );

    // And the same bytes `flint export` writes for this conversation: one renderer, two doors. The
    // number resolves to the file the run is writing -- there is one conversation in this home and it
    // is this one -- so a page that differed would mean the door had grown a renderer of its own.
    let other = work.join("from-cli.html");
    let cli = binary()
        .current_dir(&work)
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .args(["export", "1", "--out"])
        .arg(&other)
        .output()
        .expect("failed to run flint export");
    assert!(
        cli.status.success(),
        "flint export failed: {}{}",
        String::from_utf8_lossy(&cli.stdout),
        String::from_utf8_lossy(&cli.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&other).expect("the CLI's page"),
        html,
        "the two doors wrote different pages for the same conversation"
    );

    let _ = std::fs::remove_dir_all(&home);
}

/// The two refusals, which are the two ways `/export` can have nothing to write.
///
/// With no argument the path is what is missing, and the run may not choose it: stdout is the
/// terminal here, and a name derived from the conversation would land in whatever directory the run
/// happens to be in. With `--no-session` there is no file to read, which is the sentence every door
/// that would open a conversation gets.
#[test]
fn export_refuses_a_missing_path_and_a_run_with_no_conversation() {
    let home = test_home("export-refusals", "http://127.0.0.1:1/v1");
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("working directory");

    let bare = repl_of(&home, &work, &["/export", "/exit"]);
    assert!(
        bare.contains("usage: /export <file>"),
        "a bare /export did not say what it needs: {bare:?}"
    );

    let none = repl_of_with(&home, &work, &["--no-session"], &["/export page.html", "/exit"]);
    assert!(
        none.contains("keeps no conversation"),
        "a --no-session run did not refuse /export in the words the other doors use: {none:?}"
    );

    let _ = std::fs::remove_dir_all(&home);
}

/// A write that fails is reported, and the run carries on.
///
/// The difference from `flint export`, which returns an error because it *is* the run: a slash
/// command that ended the conversation because a directory did not exist would be worse than the
/// mistake it reported. The person is told which path could not be written, nothing is created, and
/// the next line is still theirs.
#[test]
fn a_failed_export_is_reported_and_the_run_carries_on() {
    let home = test_home("export-bad-path", "http://127.0.0.1:1/v1");
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("working directory");
    let given = home.join("given.jsonl");
    std::fs::write(
        &given,
        format!(
            "{}\n",
            r#"{"type":"chat","message":{"role":"user","content":"something to export"}}"#
        ),
    )
    .expect("write the given file");
    let missing = work.join("nope").join("page.html");

    let text = repl_of(
        &home,
        &work,
        &[
            &format!("/import {}", given.display()),
            &format!("/export {}", missing.display()),
            "/help",
            "/exit",
        ],
    );

    assert!(
        text.contains("cannot write"),
        "a failed write was not reported: {text:?}"
    );
    // A short directory name, and the reason is in the output rather than in the test's taste: the
    // report is printed into the viewport, which wraps a long Windows path *inside* a word, so a
    // longer name would be split across two lines and asserting on it would be asserting on the
    // terminal's width instead of on what flint said.
    assert!(
        text.contains("nope"),
        "the report does not say which path failed: {text:?}"
    );
    assert!(!missing.exists(), "a failed export created something anyway");
    assert!(
        text.contains("/export <file>"),
        "the run did not carry on after the failed write: {text:?}"
    );

    let _ = std::fs::remove_dir_all(&home);
}

/// A file with no conversation in it is refused, and nothing is created for it.
///
/// The hand-edited case has a second half worth holding: a file may parse perfectly and still hold
/// nothing to bring in -- an empty file, or one whose lines are all events and no messages. Opening a
/// conversation for it would answer "imported" with a transcript of nothing, and leave a session in
/// the list that nobody can tell from a real one.
#[test]
fn importing_a_file_with_no_conversation_says_so_and_creates_nothing() {
    let home = test_home("import-empty", "http://127.0.0.1:1/v1");
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("working directory");
    let given = home.join("no-conversation.jsonl");
    std::fs::write(&given, format!("{}\n", meta_line("999-1"))).expect("write the given file");

    let text = repl_of(&home, &work, &[&format!("/import {}", given.display()), "/exit"]);

    assert!(
        text.contains("no conversation") || text.contains("nothing to import"),
        "/import did not say the file held nothing to import: {text:?}"
    );
    assert!(
        !text.contains("imported:"),
        "/import claimed to have imported an empty file: {text:?}"
    );
    assert_eq!(
        jsonl_files(&home.join("sessions")).len(),
        0,
        "/import created a conversation for a file that held none"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// A conversation that was copied in from a file says so when it is named later.
///
/// The record is only worth writing if something reads it: a person who resumes an imported
/// conversation and one who resumes a conversation that began here see the same line, and the line is
/// the only place that answers "whose file was this". Both doors that name a conversation carry it,
/// and they share one wording.
#[test]
fn a_resumed_import_says_where_it_came_from() {
    let home = test_home("import-resumed", "http://127.0.0.1:1/v1");
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("working directory");
    write_session(
        &home.join("sessions"),
        "20260101000000-1-777.jsonl",
        &[
            &meta_line("20260101000000-1-777"),
            r#"{"type":"import","from":"/tmp/somebody-elses/given-to-me.jsonl","from_id":"given","messages":2}"#,
            r#"{"type":"chat","message":{"role":"user","content":"the imported question"}}"#,
            r#"{"type":"chat","message":{"role":"assistant","content":"the imported answer"}}"#,
        ],
        0,
    );

    let text = repl_of(
        &home,
        &work,
        &["/resume 20260101000000-1-777", "/exit"],
    );

    assert!(
        text.contains("resumed:"),
        "resuming an imported conversation said nothing: {text:?}"
    );
    assert!(
        text.contains("imported from given-to-me.jsonl"),
        "the resumed line does not say where the conversation was copied from: {text:?}"
    );
    assert!(
        text.contains("(2 messages, model stub-model)"),
        "the resumed line does not read as a person would write it: {text:?}"
    );
    assert!(
        text.contains("this conversation was imported from given-to-me.jsonl"),
        "the resumed line does not say where the conversation came from: {text:?}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// A fork from a chosen point keeps what came before it, and leaves the original whole.
///
/// The point of the command, in one test: cutting at question 2 of 3 keeps the first exchange, the
/// question that was cut at is named so it can be asked again, and the conversation that got that far
/// keeps every byte -- a fork is a copy, which is the whole reason it is not a resume. The line the
/// branch is written with is checked in the same place, because the file is what the next reader
/// (a person, or `--resume`) has to go on.
#[test]
fn a_fork_from_a_chosen_point_keeps_what_came_before_it() {
    let home = test_home("fork-cut", "http://127.0.0.1:1/v1");
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("working directory");
    let source = write_session(
        &home.join("sessions"),
        "20260101000000-1-777.jsonl",
        &[
            &meta_line("20260101000000-1-777"),
            r#"{"type":"chat","message":{"role":"user","content":"why does the socket close early"}}"#,
            r#"{"type":"chat","message":{"role":"assistant","content":"because the peer half-closes"}}"#,
            r#"{"type":"chat","message":{"role":"user","content":"what about the retry path"}}"#,
            r#"{"type":"chat","message":{"role":"assistant","content":"it backs off twice"}}"#,
            r#"{"type":"chat","message":{"role":"user","content":"and the timeout"}}"#,
            r#"{"type":"chat","message":{"role":"assistant","content":"thirty seconds"}}"#,
        ],
        0,
    );
    let before = std::fs::read(&source).expect("read the original");

    let text = repl_of(
        &home,
        &work,
        &["/resume 20260101000000-1-777", "/fork 2", "/exit"],
    );

    assert!(
        text.contains("forked: 20260101000000-1-777.jsonl"),
        "/fork did not say which conversation it cut from: {text:?}"
    );
    assert!(
        text.contains("2 messages kept, cut at question 2 of 3: what about the retry path"),
        "/fork did not say where it cut and what it left out: {text:?}"
    );
    assert_eq!(
        std::fs::read(&source).expect("read the original again"),
        before,
        "/fork wrote to the conversation it copied; that is what --resume is for"
    );

    // The branch: a conversation of this run's own, with the first exchange in it and a line saying
    // what it was cut from -- so a reader who finds it later is not looking at a conversation that
    // began from nothing.
    let written = jsonl_files(&home.join("sessions"));
    assert_eq!(
        written.len(),
        2,
        "the fork did not leave exactly one conversation of its own: {written:?}"
    );
    let branch = written
        .iter()
        .find(|p| p.file_name().map(|n| n != "20260101000000-1-777.jsonl") == Some(true))
        .expect("the branch");
    let copy = std::fs::read_to_string(branch).expect("read the branch");
    assert!(
        copy.contains("why does the socket close early")
            && copy.contains("because the peer half-closes"),
        "the branch does not hold the conversation it kept: {copy:?}"
    );
    assert!(
        !copy.contains("what about the retry path"),
        "the branch holds the question it was cut at: {copy:?}"
    );
    assert!(
        copy.contains(r#""type":"fork""#)
            && copy.contains(r#""from_id":"20260101000000-1-777""#)
            && copy.contains(r#""kept":2"#),
        "the branch does not say what it was cut from: {copy:?}"
    );
    let lines: Vec<&str> = copy.lines().collect();
    let lineage = lines
        .iter()
        .position(|l| l.contains(r#""type":"fork""#))
        .expect("the fork line");
    let conversation = lines
        .iter()
        .position(|l| l.contains(r#""type":"chat""#))
        .expect("the first kept message");
    assert!(
        lineage < conversation,
        "the branch records where it came from below the conversation: {copy:?}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// The questions are listed rather than guessed at, and listing writes nothing.
///
/// `/fork` with no argument is the terminal's half of the picker the page offers: cutting at the
/// wrong point is a copy of a conversation made for nothing, so the one thing this command may not do
/// is choose for the person.
#[test]
fn asking_for_the_questions_lists_them_and_writes_nothing() {
    let home = test_home("fork-list", "http://127.0.0.1:1/v1");
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("working directory");
    write_session(
        &home.join("sessions"),
        "20260101000000-1-777.jsonl",
        &[
            &meta_line("20260101000000-1-777"),
            r#"{"type":"chat","message":{"role":"user","content":"why does the socket close early"}}"#,
            r#"{"type":"chat","message":{"role":"assistant","content":"because the peer half-closes"}}"#,
            r#"{"type":"chat","message":{"role":"user","content":"what about the retry path"}}"#,
        ],
        0,
    );

    let text = repl_of(
        &home,
        &work,
        &["/resume 20260101000000-1-777", "/fork", "/exit"],
    );

    assert!(
        text.contains("1. why does the socket close early")
            && text.contains("2. what about the retry path"),
        "/fork did not list the questions in this conversation: {text:?}"
    );
    assert!(
        text.contains("/fork <n> starts a new conversation cut at question n"),
        "/fork did not say how to use the list it just printed: {text:?}"
    );
    assert!(
        text.contains("2 questions"),
        "the list does not say how many questions there are: {text:?}"
    );
    assert_eq!(
        jsonl_files(&home.join("sessions")).len(),
        1,
        "asking for the list wrote a conversation"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// A fold takes the digest out of the question list, and a branch in front of the first surviving
/// question keeps it.
///
/// Two defects with one cause: a fold's summary is a `user` message (`session::compacted_message`, and
/// a `system` message in the middle of a conversation is accepted by some endpoints and refused by
/// others), so `questions` counted the digest as question 1. `/fork` then listed "the conversation
/// before this point, summarized by flint at the person's request" as a question and numbered every
/// real question one higher than the transcript the person was reading -- and the cut in front of the
/// first question the run still holds was refused as "nothing to keep" when the copy would have held
/// the summary. That cut is what the page's first value is, so the page would have offered a branch
/// button whose command always answered with a refusal.
#[test]
fn a_compacted_conversation_forks_from_the_questions_it_still_holds() {
    let home = test_home("fork-after-compact", "http://127.0.0.1:1/v1");
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("working directory");
    let sessions = home.join("sessions");

    let kept = r#"{"type":"chat","message":{"role":"user","content":"the kept question"}}"#;
    let mut body = String::new();
    body.push_str(&meta_line("333-3"));
    body.push('\n');
    body.push_str(r#"{"type":"chat","message":{"role":"user","content":"the folded question"}}"#);
    body.push('\n');
    body.push_str(r#"{"type":"chat","message":{"role":"assistant","content":"the folded answer"}}"#);
    body.push('\n');
    body.push_str(kept);
    body.push('\n');
    body.push_str(r#"{"type":"chat","message":{"role":"assistant","content":"the kept answer"}}"#);
    body.push('\n');
    // The pointer computed the way a person editing this line by hand would: the byte offset of the
    // line the fold keeps. `docs/session-format.md` is the promise that this is enough.
    let from = body.find(kept).expect("the line the fold keeps");
    body.push_str(&format!(
        r#"{{"type":"compact","summary":"FOLDED SUMMARY","from":{from}}}"#
    ));
    write_session(&sessions, "333-3.jsonl", &[&body], 10);

    let text = repl_of(
        &home,
        &work,
        &["/resume 333-3", "/fork", "/fork 1", "/exit"],
    );

    assert!(
        text.contains("1. the kept question"),
        "the digest is still being listed as a question, or the questions are numbered from it: \
         {text:?}"
    );
    assert!(
        text.contains("1 question"),
        "the list counts the digest as something the person asked: {text:?}"
    );
    assert!(
        text.contains("1 message kept, cut at question 1 of 1: the kept question"),
        "cutting in front of the first question a folded run holds was refused or cut elsewhere: \
         {text:?}"
    );

    // The branch itself: one file more, and it holds the digest and nothing that came after it.
    let written = jsonl_files(&sessions);
    assert_eq!(
        written.len(),
        2,
        "the fork did not write a conversation of its own: {written:?}"
    );
    let branch = written
        .iter()
        .find(|path| {
            !path
                .file_name()
                .map(|n| n.to_string_lossy().contains("333-3"))
                .unwrap_or(false)
        })
        .expect("the branch's file");
    let carried = std::fs::read_to_string(branch).expect("the branch's file");
    assert!(
        carried.contains("FOLDED SUMMARY"),
        "the branch dropped the summary the fold stands for: {carried}"
    );
    assert!(
        !carried.contains("the kept question") && !carried.contains("the kept answer"),
        "the branch kept what the cut was in front of: {carried}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// Three refusals, and each of them is a decision rather than an error path.
///
/// Cutting in front of everything the run holds leaves a conversation with nothing in it -- a file that
/// would sit in the list looking real, which is the same fault the empty import is refused for. In an
/// unfolded conversation that is question 1; after a fold the same cut keeps the summary and is allowed
/// (`a_compacted_conversation_forks_from_the_questions_it_still_holds`), which is why the refusal reads
/// the copy rather than the number. A number past the end is answered with the range rather than an
/// empty copy. And a run that promised to keep nothing does not create a conversation: the check is the
/// same sentence every other door gets, and it is checked *first*, before anything has been said -- so
/// `--no-session` is a property of the run and not of the file it was aimed at.
#[test]
fn a_fork_that_would_keep_nothing_is_refused() {
    let home = test_home("fork-refusals", "http://127.0.0.1:1/v1");
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("working directory");
    write_session(
        &home.join("sessions"),
        "20260101000000-1-777.jsonl",
        &[
            &meta_line("20260101000000-1-777"),
            r#"{"type":"chat","message":{"role":"user","content":"the only question"}}"#,
            r#"{"type":"chat","message":{"role":"assistant","content":"the only answer"}}"#,
        ],
        0,
    );

    let text = repl_of(
        &home,
        &work,
        &[
            "/resume 20260101000000-1-777",
            "/fork 1",
            "/fork 4",
            "/fork soon",
            "/exit",
        ],
    );

    assert!(
        text.contains("nothing to keep"),
        "cutting at the first question of an unfolded conversation was allowed, or the refusal no \
         longer names the cut: {text:?}"
    );
    assert!(
        text.contains("no question 4") && text.contains("asked 1 question"),
        "/fork did not answer a number past the end with the range: {text:?}"
    );
    assert!(
        text.contains("usage: /fork <n>"),
        "a word where a number belongs got no usage line: {text:?}"
    );
    assert_eq!(
        jsonl_files(&home.join("sessions")).len(),
        1,
        "a refused fork created a conversation"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// A branch says what it was cut from when it is named later.
///
/// The file records it and one function prints it, so both doors that name a conversation -- the
/// startup `resumed` line and `/resume` -- say the same thing. It is the import test's claim one step
/// over: a record nothing reads is damage, and a branch is exactly the conversation a person comes
/// back to a week later wanting to know where it came from.
#[test]
fn a_resumed_branch_says_what_it_was_cut_from() {
    let home = test_home("fork-resumed", "http://127.0.0.1:1/v1");
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("working directory");
    write_session(
        &home.join("sessions"),
        "20260101000000-1-777.jsonl",
        &[
            &meta_line("20260101000000-1-777"),
            r#"{"type":"fork","from":"/home/somebody/.flint/sessions/20251231000000-1-9.jsonl","from_id":"20251231000000-1-9","kept":4}"#,
            r#"{"type":"chat","message":{"role":"user","content":"the first question"}}"#,
            r#"{"type":"chat","message":{"role":"assistant","content":"the first answer"}}"#,
        ],
        0,
    );

    let text = repl_of(
        &home,
        &work,
        &["/resume 20260101000000-1-777", "/exit"],
    );

    assert!(
        text.contains("(2 messages, model stub-model)"),
        "the resumed line does not read as a person would write it: {text:?}"
    );
    assert!(
        text.contains("this conversation was forked from 20251231000000-1-9.jsonl, and holds the first 4 messages"),
        "the resumed line does not say what the conversation was cut from: {text:?}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// `--fork` records its lineage too: it is the same act, taken before the conversation is open.
///
/// The startup path copied a conversation into a new file and said nothing in that file about where it
/// came from, which made a forked conversation indistinguishable from one that began here -- the gap
/// the in-run fork closed, closed on both doors at once because it is one function that writes the
/// line.
#[test]
fn forking_at_startup_records_where_the_copy_came_from() {
    let home = test_home("fork-startup", "http://127.0.0.1:1/v1");
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("working directory");
    write_session(
        &home.join("sessions"),
        "20260101000000-1-777.jsonl",
        &[
            &meta_line("20260101000000-1-777"),
            r#"{"type":"chat","message":{"role":"user","content":"the question from yesterday"}}"#,
            r#"{"type":"chat","message":{"role":"assistant","content":"the answer from yesterday"}}"#,
        ],
        0,
    );

    let text = repl_of_with(
        &home,
        &work,
        &["--fork", "20260101000000-1-777"],
        &["/exit"],
    );
    assert!(
        text.contains("forking 20260101000000-1-777.jsonl"),
        "the startup fork did not say what it was copying: {text:?}"
    );

    let written = jsonl_files(&home.join("sessions"));
    let copy = written
        .iter()
        .find(|p| p.file_name().map(|n| n != "20260101000000-1-777.jsonl") == Some(true))
        .expect("the forked copy");
    let branch = std::fs::read_to_string(copy).expect("read the copy");
    assert!(
        branch.contains(r#""type":"fork""#)
            && branch.contains(r#""from_id":"20260101000000-1-777""#),
        "a startup fork did not record where the copy came from: {branch:?}"
    );
    assert!(
        !branch.contains(r#""kept""#),
        "a copy of the whole conversation claims a cut: {branch:?}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// A run that keeps no conversation is offered no cut, even though it has questions.
///
/// The page's dialog is drawn from the frame, so a value in that frame is a button a person can press.
/// A `--no-session` run has questions in its history and no file to cut a branch out of: `/fork` refuses
/// it in the same words every other door gets, and the terminal checks that *first*, before listing
/// anything. The frame has to say the same thing -- the page cannot know the run keeps no conversation,
/// and a frame offering the questions would be offering presses whose only outcome is that refusal.
///
/// A question is asked first, because an empty list over an empty conversation would prove nothing.
/// The frame under test is the one *after* that, and it is reached through a command that changes the
/// frame on purpose (`/verbose off`, a setting the frame carries): a frame whose `commands` have not
/// changed is dropped as unchanged, so waiting for "the next state frame" after a question would wait
/// for a frame a correct build never sends -- twenty seconds of nothing, and an assertion that passed
/// because it had no frame to look at. Asking for a frame that must come is what makes the absence of
/// `labels` mean the absence of the pair a branch button is drawn from.
#[tokio::test]
async fn a_run_that_keeps_no_conversation_offers_the_page_no_cut() {
    use std::io::Write;

    let server = MockServer::start().await;
    answer_once(&server).await;
    let home = test_home("no-session-picker", &server.uri());
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("working directory");
    let log = home.join("transcript.txt");
    let errors = home.join("stderr.txt");
    let mut child = binary()
        .arg("--web")
        .arg("--no-session")
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .current_dir(&work)
        .stdin(std::process::Stdio::piped())
        .stdout(std::fs::File::create(&log).expect("transcript file"))
        .stderr(std::fs::File::create(&errors).expect("stderr file"))
        .spawn()
        .expect("failed to run flint");

    let (port, token) = port_and_token(&wait_for_url(&log));
    let mut watching = http_stream(port, "/events", &token);
    let opening = read_until(&mut watching, "\"type\":\"state\"", 20);
    let mut say = |line: &str| {
        child
            .stdin
            .as_mut()
            .expect("no stdin handle")
            .write_all(line.as_bytes())
            .expect("failed to write stdin");
    };
    say("why does the socket close early\n");
    wait_for_requests(&server, 1).await;
    let asked = read_until(&mut watching, "\"type\":\"turn.completed\"", 20);
    // And now a frame that has to be sent: the setting it carries moves.
    say("/verbose off\n");
    let after = read_until(&mut watching, "\"type\":\"state\"", 20);
    drop(watching);
    drop(child.stdin.take());
    let exited = wait_for_exit(&mut child, 20);
    let _ = std::fs::remove_dir_all(&home);
    assert!(exited, "flint did not exit");

    assert!(
        asked.contains("why does the socket close early"),
        "the run never got the question this test is about, so the frame proves nothing: {asked:?}"
    );
    assert!(
        after.contains("\"value\":\"off\"") && !after.contains("\"value\":\"on\""),
        "the frame under test is not the one the command moved, so it proves nothing: {after:?}"
    );
    // `labels` is the field only `/fork`'s row carries, so its absence is the absence of the pair a
    // branch button is drawn from -- and the assertion says nothing about key order within a row.
    assert!(
        !opening.contains("\"labels\""),
        "a run with nothing asked yet was offered questions: {opening:?}"
    );
    assert!(
        !after.contains("\"labels\""),
        "a --no-session run offered the page cuts it would refuse, on a question it does hold: {after:?}"
    );
}

/// The page is offered the arguments a command takes: a list to choose from, and a sentence to write.
///
/// `/fork` is the one whose values are about *this conversation* rather than about the machine: the
/// questions a person asked here, in order. The frame is rebuilt as the conversation moves, so a page
/// open since the second question does not offer a third until it has been asked — which is the same
/// property the sidebar's conversation list has, and the reason the values are computed rather than
/// kept. The value is the **ordinal alone**: the page composes `/<name> <value>`, so a value carrying
/// the question's own text would be sent as part of the command line.
///
/// `/queue` is the other shape, and it is in the same test because the two are one claim about one
/// frame: what a command takes, the page is handed. A field is all a follow-up can be — the page's
/// composer sends a line, and a line cannot say "not yet" unless a command does.
#[tokio::test]
async fn the_page_is_offered_the_arguments_a_command_takes() {
    let server = MockServer::start().await;
    let home = test_home("fork-picker", &server.uri());
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("working directory");
    write_session(
        &home.join("sessions"),
        "20260101000000-1-777.jsonl",
        &[
            &meta_line("20260101000000-1-777"),
            r#"{"type":"chat","message":{"role":"user","content":"why does the socket close early"}}"#,
            r#"{"type":"chat","message":{"role":"assistant","content":"because the peer half-closes"}}"#,
            r#"{"type":"chat","message":{"role":"user","content":"what about the retry path"}}"#,
        ],
        0,
    );
    let log = home.join("transcript.txt");
    // The run's stderr, kept rather than sent to `/dev/null`: a `--web` run that dies mid-test
    // otherwise reports a connection that was refused, or a transcript that simply stops, and
    // nothing about *why* -- the class the flaky jobs test came from, which `HANDOFF.md` names.
    // It sits in this test's own home, and a test that panics leaves the whole home behind.
    let errors = home.join("stderr.txt");
    let mut child = binary()
        .arg("--web")
        .args(["--resume", "20260101000000-1-777"])
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .current_dir(&work)
        .stdin(std::process::Stdio::piped())
        .stdout(std::fs::File::create(&log).expect("transcript file"))
        .stderr(std::fs::File::create(&errors).expect("stderr file"))
        .spawn()
        .expect("failed to run flint");

    let (port, token) = port_and_token(&wait_for_url(&log));
    let mut watching = http_stream(port, "/events", &token);
    // The frame the page is handed before anything happens, which is where its rows are drawn from.
    let opening = read_until(&mut watching, "\"type\":\"state\"", 20);
    drop(watching);
    drop(child.stdin.take());
    let exited = wait_for_exit(&mut child, 20);
    let _ = std::fs::remove_dir_all(&home);

    assert!(exited, "flint did not exit");
    let commands = opening
        .split("\"commands\":[")
        .nth(1)
        .and_then(|rest| rest.split("],\"model\"").next())
        .unwrap_or_default();
    assert!(
        commands.contains("\"send\":\"/fork\""),
        "the page has no row to cut this conversation at: {commands:?}"
    );
    assert!(
        commands.contains("\"values\":[\"1\",\"2\"]"),
        "the row does not carry the questions this conversation was asked: {commands:?}"
    );
    // And what those numbers are *of*: a value that is a question number says nothing about what is
    // being cut, so the question's own first line travels beside it -- the settings screen draws it on
    // the button, and the transcript finds the turn to hang the branch button on by matching it. The
    // two lists are one read of `questions`, so they cannot come apart.
    assert!(
        commands.contains(
            "\"labels\":[\"why does the socket close early\",\"what about the retry path\"]"
        ),
        "the page is given the question numbers without the questions: {commands:?}"
    );
    // The other kind of argument a command takes: a sentence, which the page composes into
    // `/queue <text>`. Without this row the page would have no way to send a follow-up at all -- a
    // composer can send a line, and "after the turn, not now" is not something a line can say unless a
    // command says it -- so the form is what makes the page's half of the feature exist.
    assert!(
        commands.contains("\"send\":\"/queue\"")
            && commands.contains("\"class\":\"form\"")
            && commands.contains("\"name\":\"text\",\"optional\":false"),
        "the page cannot ask for a line to be sent after the turn: {commands:?}"
    );
}

/// Drive the REPL from a working directory of its own, with no model behind it.
///
/// `repl` runs the child in the test process's working directory, which is this checkout -- a run
/// started there reads *this* repository's `AGENTS.md` and belongs to this directory's sessions, and
/// a test that means to hold one line of output does not want either. No request is made here: every
/// line handed to it is a command, and the provider in the config is a port that refuses.
fn repl_of(home: &std::path::Path, cwd: &std::path::Path, lines: &[&str]) -> String {
    repl_of_with(home, cwd, &[], lines)
}

/// `repl_of` with flags, for the commands whose whole subject is a flag (`--fork`).
fn repl_of_with(
    home: &std::path::Path,
    cwd: &std::path::Path,
    args: &[&str],
    lines: &[&str],
) -> String {
    use std::io::Write;
    let mut command = binary();
    command
        .args(args)
        .current_dir(cwd)
        .env("FLINT_HOME", home)
        .env("FLINT_TERM_CAPTURE", "1")
        .env("FLINT_TERM_SIZE", "100x24")
        .env_remove("NO_COLOR")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command.spawn().expect("failed to run flint");
    {
        let stdin = child.stdin.as_mut().expect("no stdin handle");
        stdin
            .write_all(format!("{}\n", lines.join("\n")).as_bytes())
            .expect("failed to write stdin");
    }
    let out = child.wait_with_output().expect("flint did not finish");
    let mut text = String::from_utf8_lossy(&out.stdout).to_string();
    // The startup lines (`flint: forking …`, `flint: forked into …`) are on stderr, and a test about
    // a flag has to be able to see what the flag said.
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    text
}

/// Drive a real REPL with one line typed into it: what it drew, and what the model was sent.
///
/// `files` are written under the home before the run starts, which is where a prompt file lives
/// (`<FLINT_HOME>/prompts/<name>.md`), and the run's working directory is `<home>/work` so that a
/// project's own copy (`<cwd>/.flint/prompts/`) can be a fixture too. The pipe is closed after the
/// line rather than `/exit` being typed, because `/exit` arrives while the turn is still running and
/// is now run as the command it is -- which cancels the turn whose request this reads.
async fn typed_at_a_repl(
    server: &MockServer,
    tag: &str,
    files: &[(&str, &str)],
    typed: &str,
) -> (String, serde_json::Value) {
    let home = test_home(tag, &server.uri());
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("working directory");
    for (rel, body) in files {
        let path = home.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("a directory for the fixture");
        }
        std::fs::write(&path, body).expect("write the fixture");
    }

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
        child
            .stdin
            .as_mut()
            .expect("no stdin handle")
            .write_all(typed.as_bytes())
            .expect("failed to write stdin");
    }
    let out = child.wait_with_output().expect("flint did not finish");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();

    let requests = server.received_requests().await.expect("requests");
    let body = requests
        .first()
        .map(|r| serde_json::from_slice(&r.body).expect("the request body is JSON"))
        .unwrap_or_else(|| {
            panic!("no request was made, so nothing typed at the prompt reached the model. The run said: {stdout:?}")
        });
    let _ = std::fs::remove_dir_all(&home);
    (stdout, body)
}

/// A prompt file is a saved prompt: typing its name sends what the file holds.
///
/// The half that makes it worth having is the arguments -- a prompt worth saving is usually one
/// with a hole in it -- and the half that makes it usable is that the *transcript* still shows the
/// line the person typed. Echoing three paragraphs of file instead would make every invocation a
/// screenful, and the person already knows what they saved.
#[tokio::test]
async fn a_prompt_file_is_sent_when_its_name_is_typed() {
    let server = MockServer::start().await;
    answer_once(&server).await;
    let (stdout, sent) = typed_at_a_repl(
        &server,
        "prompt-typed",
        &[(
            "prompts/tidy-commits.md",
            "---\ndescription: Squash and reword the commits.\n---\n\n\
             Tidy the commits touching {args}, then say what changed.\n",
        )],
        "/tidy-commits src/parser.rs\n",
    )
    .await;

    assert!(
        sent.to_string()
            .contains("Tidy the commits touching src/parser.rs, then say what changed."),
        "the file's words, with the line's arguments in the hole, are what the model must be sent: \
         {sent}"
    );
    assert!(
        stdout.contains("/tidy-commits src/parser.rs"),
        "the transcript must echo the line the person typed: {stdout:?}"
    );
    assert!(
        !stdout.contains("Tidy the commits touching"),
        "the expansion was echoed as though the person had typed it: {stdout:?}"
    );
}

/// A skill can be invoked by the person, not only loaded by the model.
///
/// The `skill` tool is the model's door and it has always been the only one: `/skills <name>` prints
/// the body, which is reading, not doing. So a catalog flint hopes the model consults was a set of
/// instructions nobody else could aim at anything.
#[tokio::test]
async fn a_skill_can_be_invoked_by_the_person() {
    let server = MockServer::start().await;
    answer_once(&server).await;
    let (stdout, sent) = typed_at_a_repl(
        &server,
        "skill-invoked",
        &[(
            "work/.flint/skills/tidy-commits/SKILL.md",
            "---\nname: tidy-commits\ndescription: Squash and reword the commits.\n---\n\n\
             Squash the commits on this branch and reword each one.\n",
        )],
        "/skill tidy-commits only the last three\n",
    )
    .await;

    assert!(
        sent.to_string()
            .contains("Squash the commits on this branch and reword each one."),
        "the skill's body is what the person's turn must carry: {sent}"
    );
    assert!(
        sent.to_string().contains("only the last three"),
        "what the person typed after the name was dropped: {sent}"
    );
    assert!(
        stdout.contains("/skill tidy-commits only the last three"),
        "the transcript must echo the line the person typed: {stdout:?}"
    );
}

/// The prompt files are listed, with the description that says what each is for.
///
/// A saved prompt nobody can find is a saved prompt nobody uses, and the listing is the only door
/// onto the names: the catalog the model is given must not grow a line per template, because a
/// template is not the model's to invoke.
#[tokio::test]
async fn prompt_files_are_listed_with_their_descriptions() {
    let server = MockServer::start().await;
    let home = test_home("prompt-listing", &server.uri());
    std::fs::create_dir_all(home.join("prompts")).expect("prompt directory");
    std::fs::write(
        home.join("prompts/tidy-commits.md"),
        "---\ndescription: Squash and reword the commits.\n---\n\nTidy the commits.\n",
    )
    .expect("write the template");

    let mut child = binary()
        .current_dir(&home)
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
        child
            .stdin
            .as_mut()
            .expect("no stdin handle")
            .write_all(b"/prompts\n/exit\n")
            .expect("failed to write stdin");
    }
    let out = child.wait_with_output().expect("flint did not finish");
    let text = String::from_utf8_lossy(&out.stdout).to_string();

    assert!(
        text.contains("tidy-commits"),
        "/prompts listed none of the templates: {text:?}"
    );
    assert!(
        text.contains("Squash and reword the commits."),
        "the listing is a name and nothing else, so nobody can tell which one to type: {text:?}"
    );
    assert!(
        text.contains("prompts"),
        "the listing does not say where it looked: {text:?}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// The turn footer carries the cache rate, and an endpoint that reports none gets no cache text.
///
/// The footer is where counts are read turn by turn -- it is printed under every answer -- so a rate
/// that only existed in `/usage` would be a number nobody looks at. The control half is the one that
/// matters: an endpoint with no cache split must produce no `cache` text at all, because `0%` would
/// read as "the prompt flint builds keeps changing" when the truth is "this endpoint does not say".
#[tokio::test]
async fn the_turn_footer_carries_the_cache_rate() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse(&[
                    r#"data: {"choices":[{"delta":{"content":"STUB ANSWER"}}]}"#,
                    r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
                    r#"data: {"choices":[],"usage":{"prompt_tokens":1000,"completion_tokens":7,"prompt_cache_hit_tokens":871}}"#,
                    "data: [DONE]",
                ])),
        )
        .mount(&server)
        .await;
    // The tags name no cache, deliberately: the scratch home's path is printed on the run's first
    // line, so a tag containing the word would be the thing the control below found on screen.
    let (stdout, _) = typed_at_a_repl(&server, "hit-footer", &[], "hello\n").await;
    assert!(
        stdout.contains("1000 prompt + 7 completion") && stdout.contains("87% cached"),
        "the footer must carry the cache rate beside the counts it is a share of: {stdout:?}"
    );

    // The control reports counts and no split at all, which is the shape most endpoints have: the
    // footer must still be there, and the word `cache` must not be on it.
    let quiet = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse(&[
                    r#"data: {"choices":[{"delta":{"content":"STUB ANSWER"}}]}"#,
                    r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
                    r#"data: {"choices":[],"usage":{"prompt_tokens":1000,"completion_tokens":7}}"#,
                    "data: [DONE]",
                ])),
        )
        .mount(&quiet)
        .await;
    let (stdout, _) = typed_at_a_repl(&quiet, "hit-footer-quiet", &[], "hello\n").await;
    assert!(
        stdout.contains("1000 prompt + 7 completion") && !stdout.contains("cache"),
        "a provider that reports no cache split must not have the word on screen, and must still \
         have its counts: {stdout:?}"
    );
}

/// `/usage` prints the split beside the prompt count, and only when the session's provider said so.
///
/// Driven from a *resumed* session rather than a live turn, and that is the stronger test of the two:
/// the number a person asks for with `/usage` is read back out of the session file, so this also holds
/// the format -- a usage line written by one run, with the cache split on it, still means the same
/// thing to the next one. It is also race-free: nothing is typed during a turn.
#[tokio::test]
async fn usage_prints_the_cache_split_it_read_back_from_the_session() {
    let server = MockServer::start().await;
    // Again no cache in the tag: the scratch home is the run's working directory and its path is on
    // the first line of the capture.
    let home = test_home("hit-usage", &server.uri());
    let dir = home.join("sessions");
    std::fs::create_dir_all(&dir).expect("sessions directory");
    let with_cache = write_session(
        &dir,
        "111-1.jsonl",
        &[
            &meta_line("111"),
            r#"{"type":"chat","message":{"role":"user","content":"what is the socket question"}}"#,
            r#"{"type":"usage","usage":{"prompt_tokens":1000,"completion_tokens":7,"cache_hit_tokens":871}}"#,
        ],
        5,
    );
    let without = write_session(
        &dir,
        "222-1.jsonl",
        &[
            &meta_line("222"),
            r#"{"type":"chat","message":{"role":"user","content":"what is the socket question"}}"#,
            r#"{"type":"usage","usage":{"prompt_tokens":1000,"completion_tokens":7}}"#,
        ],
        5,
    );

    let said = |session: &std::path::Path| {
        let mut child = binary()
            .current_dir(&home)
            .env("FLINT_HOME", &home)
            .env("FLINT_TERM_CAPTURE", "1")
            .env("FLINT_TERM_SIZE", "100x24")
            .env_remove("NO_COLOR")
            .args(["--resume", &session.to_string_lossy()])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("failed to run flint");
        {
            use std::io::Write;
            child
                .stdin
                .as_mut()
                .expect("no stdin handle")
                .write_all(b"/usage\n/exit\n")
                .expect("failed to write stdin");
        }
        let out = child.wait_with_output().expect("flint did not finish");
        String::from_utf8_lossy(&out.stdout).to_string()
    };

    let shown = said(&with_cache);
    assert!(
        shown.contains("871 of 1000 prompt tokens") && shown.contains("87%"),
        "the cached count and the rate it makes belong on the same line as the prompt: {shown:?}"
    );
    let silent = said(&without);
    assert!(
        silent.contains("1000") && !silent.contains("cache"),
        "a session whose provider reported no split must say nothing about caching: {silent:?}"
    );
    let _ = std::fs::remove_dir_all(&home);
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
/// read *keys*, and the paste fix splits along exactly that line: the **ask** that makes a terminal
/// wrap a paste is asserted from a run's own bytes here
/// (`the_terminal_is_asked_to_wrap_a_paste`, which needs the capture hook rather than a pty), while
/// what the handler does with the paste it then receives still needs a pty to deliver -- this helper
/// reads lines and never touches the event reader (see `HANDOFF.md`).
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

/// The terminal is asked to wrap a paste, which is the half of the paste fix no unit test could reach.
///
/// `Event::Paste` had a handler and two unit tests for as long as it existed, and in a real terminal the
/// branch was *unreachable*: a terminal only wraps a paste in `\x1b[200~ … \x1b[201~` when the program
/// asks it to, and nothing ever asked. A three-line paste therefore arrived one keystroke per character
/// with an Enter per newline -- three messages, the second and third interrupting the first through the
/// steering path -- while the handler's test asserted a shape nothing could deliver. `HANDOFF.md` records
/// that fix as verified by hand in a pty, and its own words are why this test exists: "the *enable*
/// itself is only checked there". Now it is checked here, from the bytes a real process writes, so
/// deleting the ask fails in the gate rather than in somebody's session.
///
/// What stays where it was: the handler's two shapes are held by the unit tests beside it, and a paste
/// cannot be delivered through a pipe at all -- `from_stdin` reads lines and never touches the event
/// reader -- so the end-to-end shape is still only reachable under a real pty. The enable is the part
/// that can be held here, and it is the part that was missing. Like every capture test this needs a debug
/// build (`FLINT_TERM_CAPTURE` is compiled out of a release one on purpose), which is what `cargo test`
/// builds.
#[tokio::test]
async fn the_terminal_is_asked_to_wrap_a_paste() {
    // No model is reached: the run is given `/exit` and never has a turn. The provider exists so that
    // starting up is the ordinary path rather than a refusal.
    let home = test_home("bracketed-paste", "http://127.0.0.1:9/v1");
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
        stdin.write_all(b"/exit\n").expect("failed to write stdin");
    }
    let out = child.wait_with_output().expect("flint did not finish");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let _ = std::fs::remove_dir_all(&home);

    assert!(
        text.contains("\u{1b}[?2004h"),
        "the run never asked the terminal to wrap a paste, so a pasted block arrives as one message per \
         line and the `Event::Paste` handler below it can never run: {text:?}"
    );
    // The interactive path is what the enable belongs to, so its absence would make the assertion above
    // pass on some other line: this is the same stream the viewport draws itself with.
    assert!(
        text.contains("\u{1b}[2K"),
        "the capture did not take the interactive path, so this test is not measuring a terminal: {text:?}"
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
///
/// The hold is either indefinite (`start`, for a turn that is waiting to be interrupted) or a
/// delay after which the first answer *finishes* (`start_finishing`, for a turn that must be
/// allowed to end on its own). The second mode carries the flag the queue test needs: whether
/// the first response ever delivered its last frame, which is how "the follow-up did not
/// interrupt the turn" becomes something the stub can witness rather than something the test
/// infers from timing.
struct HangingProvider {
    base_url: String,
    bodies: std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
    first_finished: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl HangingProvider {
    /// Draw the answer and hold the connection open until the client gives up.
    fn start(drawn: &'static str) -> Self {
        Self::with_finish(drawn, None)
    }

    /// Draw the answer, then finish it after `after` -- so a turn can run to completion while
    /// something is typed into it.
    fn start_finishing(drawn: &'static str, after: std::time::Duration) -> Self {
        Self::with_finish(drawn, Some(after))
    }

    fn with_finish(drawn: &'static str, finish_after: Option<std::time::Duration>) -> Self {
        use std::io::Write;

        let listener =
            std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind a stub provider");
        let base_url = format!("http://{}", listener.local_addr().expect("addr"));
        let bodies = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let first_finished = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let seen = bodies.clone();
        let finished = first_finished.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut sock) = stream else { continue };
                let seen = seen.clone();
                let finished = finished.clone();
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
                        match finish_after {
                            // Held open: the turn stays in flight until the client gives up.
                            None => {
                                std::thread::sleep(std::time::Duration::from_secs(20));
                            }
                            // Finished on its own, which is what a turn nobody interrupted does.
                            // The flag is set *after* the frames are flushed, so a second request
                            // that arrives while it is still false is a second request that the
                            // client sent before this answer was over -- which is exactly what an
                            // interrupted turn looks like from here.
                            Some(after) => {
                                std::thread::sleep(after);
                                let _ = sock.write_all(concat!(
                                    "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                                    "data: [DONE]\n\n"
                                ).as_bytes());
                                let _ = sock.flush();
                                finished.store(true, std::sync::atomic::Ordering::SeqCst);
                                std::thread::sleep(std::time::Duration::from_secs(20));
                            }
                        }
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
        HangingProvider {
            base_url,
            bodies,
            first_finished,
        }
    }

    fn bodies(&self) -> Vec<String> {
        self.bodies
            .lock()
            .expect("bodies")
            .iter()
            .map(|b| String::from_utf8_lossy(b).to_string())
            .collect()
    }

    /// Whether the first response got as far as its last frame.
    fn finished_first(&self) -> bool {
        self.first_finished.load(std::sync::atomic::Ordering::SeqCst)
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

/// Every question the stub was asked, across all the requests it saw: what a failing assertion about
/// a run's turns should print instead of two whole request bodies (a system prompt is most of one).
fn questions_of(bodies: &[String]) -> String {
    bodies
        .iter()
        .enumerate()
        .map(|(i, body)| {
            let asked = serde_json::from_str::<serde_json::Value>(body)
                .ok()
                .and_then(|b| {
                    b["messages"].as_array().map(|messages| {
                        messages
                            .iter()
                            .filter(|m| m["role"] == "user")
                            .filter_map(|m| m["content"].as_str())
                            .collect::<Vec<_>>()
                            .join(" | ")
                    })
                })
                .unwrap_or_default();
            format!("#{}: {asked}", i + 1)
        })
        .collect::<Vec<_>>()
        .join("; ")
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
    let written: String = jsonl_files(&home.join("sessions"))
        .into_iter()
        .map(|path| std::fs::read_to_string(path).unwrap_or_default())
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

/// A follow-up waits for the turn it was queued in, and that turn is allowed to finish.
///
/// This is the whole of the difference from steering, and it is why the stub has to be able to *end*
/// an answer: the claim is not "the line was taken" but "the answer it interrupted nothing of ran to
/// its last frame, and then the line was sent". The stub records that last frame, so the test asserts
/// the turn's completion from the server's side rather than inferring it from a stopwatch.
#[tokio::test]
async fn a_follow_up_waits_for_the_turn_it_was_queued_in() {
    use std::io::Write;

    // Draws at once and finishes a moment later: the window the test types into, and the turn the
    // queue has to wait for.
    let provider = HangingProvider::start_finishing(
        "A HALF-WRITTEN ARTICLE\n",
        std::time::Duration::from_millis(1500),
    );
    let home = test_home("queue-waits", &provider.base_url);
    let (mut child, mut stdin, out_path) = mid_answer(&home);

    // Not a correction: "also, when you are done, ...". The turn keeps running.
    stdin
        .write_all(b"/queue and then check the tests\n")
        .expect("failed to write stdin");
    // Closed here, so the run exits when the *queued* turn has answered: the first turn ends on its
    // own, the queue is drained into a second turn, and stdin is already at EOF behind it.
    drop(stdin);

    let exited = wait_for_exit(&mut child, 30);
    let bodies = provider.bodies();
    let text = std::fs::read_to_string(&out_path).unwrap_or_default();
    let finished_first = provider.finished_first();
    let _ = std::fs::remove_dir_all(&home);

    assert!(exited, "flint did not exit: {text}");
    assert!(
        text.contains("queued for after this turn"),
        "the line was not taken as a follow-up, so it either interrupted the turn or went to the \
         model: {text}"
    );
    assert!(
        finished_first,
        "the turn was dropped before its answer ended, so the follow-up interrupted it: {text}"
    );
    assert!(
        bodies.len() == 2,
        "expected the turn and the follow-up it was queued for, got {} request(s): {}",
        bodies.len(),
        questions_of(&bodies)
    );
    let second = sent_to_the_model(&bodies, 1);
    assert!(
        second.contains("and then check the tests"),
        "the follow-up never became the next question: {second}"
    );
    assert!(
        second.contains("A HALF-WRITTEN ARTICLE"),
        "the answer it waited for is missing from the follow-up's request: {second}"
    );
}

/// A stop ends the answer in flight and not what somebody typed next.
///
/// Pi's rule, and the half of its queue worth copying (`docs/pi-agent-harness.md` §3.2): aborting
/// *continues* the messages still queued, and discarding them is a separate act. flint has no separate
/// act -- see `dropped_queue` -- so this is the property that keeps `/queue` from being a trap: the
/// line is either sent or said to be dropped, never quietly gone.
#[tokio::test]
async fn a_stop_does_not_throw_away_a_queued_follow_up() {
    use std::io::Write;

    let provider = HangingProvider::start("A HALF-WRITTEN ARTICLE\n");
    let home = test_home("queue-outlives-stop", &provider.base_url);
    let (mut child, mut stdin, out_path) = mid_answer(&home);

    // Both lines while the turn is in flight, in the order a person types them: the follow-up, and
    // then the decision to stop the answer it was waiting for.
    stdin
        .write_all(b"/queue and then check the tests\n/stop\n")
        .expect("failed to write stdin");
    drop(stdin);

    let exited = wait_for_exit(&mut child, 30);
    let bodies = provider.bodies();
    let text = std::fs::read_to_string(&out_path).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&home);

    assert!(exited, "flint did not exit: {text}");
    assert!(
        text.contains("queued for after this turn"),
        "the line was not taken as a follow-up: {text}"
    );
    assert!(
        bodies.len() == 2,
        "the stop took the queued line with it, or nothing was queued: got {} request(s): {}",
        bodies.len(),
        questions_of(&bodies)
    );
    let second = sent_to_the_model(&bodies, 1);
    assert!(
        second.contains("and then check the tests"),
        "the stop threw the queued line away: {second}"
    );
    assert!(
        !text.contains("dropped"),
        "the queue was reported as dropped, which a stop must not do: {text}"
    );
}

/// The same command with nothing running: there is no turn to hold it for, so it is sent now.
///
/// The alternative -- refusing, and making the person retype it -- would be a command with a rule
/// about *when* it may be said, and the check that told them would be a line of output they could
/// have spent sending the sentence. So the note is the honest half: the transcript says the line went
/// now rather than being held.
#[tokio::test]
async fn a_queue_with_nothing_running_is_this_run_s_message() {
    let server = MockServer::start().await;
    answer_once(&server).await;
    let (stdout, sent) = typed_at_a_repl(
        &server,
        "queue-idle",
        &[],
        "/help\n/queue and then check the tests\n",
    )
    .await;

    assert!(
        sent.to_string().contains("and then check the tests"),
        "the line was held for a turn that was never coming: {sent}"
    );
    assert!(
        stdout.contains("nothing is running, so it is this turn's message"),
        "the transcript does not say the line was sent rather than queued: {stdout:?}"
    );
    assert!(
        stdout.contains("/queue <text>"),
        "the command is not in the help table, which is how anybody finds it: {stdout:?}"
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

/// Drive one of the commands that rebuilds the agent, and report what the model was sent.
///
/// The conversation is driven a step at a time. The line *after* a switch is only interesting
/// if the line before it had already been sent, so the test waits for the request rather than
/// for a stopwatch -- the stub's own record of what it received says when that happened, and a
/// sleep would be a guess about a machine this test does not control.
///
/// Returns the terminal text, the session files as (name, contents), every request body, and the
/// config file as the run left it. The config is here because one of the commands that rebuilds the
/// agent *writes* it (`/config set`), and this helper removes the home directory before it returns --
/// so a caller that read the file afterwards would be reading a path that is already gone, which is a
/// failure that looks exactly like the command not having written anything.
async fn through_a_switch(
    server: &MockServer,
    tag: &str,
    config: &str,
    command: &str,
) -> (String, Vec<(String, String)>, Vec<String>, String) {
    use std::io::Write;

    let home = test_home(tag, &server.uri());
    std::fs::write(home.join("config.toml"), config).expect("failed to write the test config");
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("working directory");
    let sessions = home.join("sessions");
    write_session(
        &sessions,
        "111-1.jsonl",
        &[
            &meta_line("111-1"),
            r#"{"type":"chat","message":{"role":"user","content":"the earlier question"}}"#,
            r#"{"type":"chat","message":{"role":"assistant","content":"the earlier answer"}}"#,
        ],
        10,
    );

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
        let stdin = child.stdin.as_mut().expect("no stdin handle");
        // By id, not by list number: this run opened a session of its own at startup, and
        // that one is the newest.
        stdin
            .write_all(b"/resume 111-1\nthe question in this run\n")
            .expect("failed to write stdin");
    }

    wait_for_requests(server, 1).await;
    {
        let stdin = child.stdin.as_mut().expect("no stdin handle");
        stdin
            .write_all(format!("{command}\nand now?\n").as_bytes())
            .expect("failed to write stdin");
    }
    let out = child.wait_with_output().expect("flint did not finish");

    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let bodies = server
        .received_requests()
        .await
        .expect("requests")
        .iter()
        .map(|request| String::from_utf8_lossy(&request.body).to_string())
        .collect();
    // Read through `jsonl_files`, so the session is found wherever the layout puts it: a test that
    // looked only in the sessions root would report that nothing was written at all.
    let files = jsonl_files(&sessions)
        .into_iter()
        .map(|path| {
            (
                path.file_name().unwrap_or_default().to_string_lossy().to_string(),
                std::fs::read_to_string(&path).unwrap_or_default(),
            )
        })
        .collect();
    // Read before the home goes: this is the file `/config set` writes, and the only evidence that
    // the change reached the disk rather than the transcript.
    let written = std::fs::read_to_string(home.join("config.toml")).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&home);
    (text, files, bodies, written)
}

/// The `.jsonl` files under a sessions directory, sorted by name so a comparison is about the files
/// rather than about the order the filesystem happened to hand them over in.
///
/// One level down is included, and that is not a convenience: a conversation lives in the
/// subdirectory belonging to the working directory it was held in, so a test that looked only in the
/// root would find nothing and say the run wrote no session. The archive is skipped, because it is
/// out of every listing by definition and a test asking "how many sessions are there" means the
/// ones that are still in play.
fn jsonl_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out: Vec<std::path::PathBuf> = Vec::new();
    collect_jsonl(dir, &mut out);
    for entry in std::fs::read_dir(dir).expect("the sessions directory").flatten() {
        let path = entry.path();
        if path.is_dir() && path.file_name().and_then(|n| n.to_str()) != Some("archive") {
            collect_jsonl(&path, &mut out);
        }
    }
    out.sort();
    out
}

fn collect_jsonl(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    out.extend(
        std::fs::read_dir(dir)
            .expect("the sessions directory")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.extension().map(|e| e == "jsonl").unwrap_or(false)),
    );
}

/// Wait until the stub has received `n` requests, without blocking the runtime it runs on.
async fn wait_for_requests(server: &MockServer, n: usize) {
    for _ in 0..200 {
        let seen = server
            .received_requests()
            .await
            .map(|requests| requests.len())
            .unwrap_or(0);
        if seen >= n {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("the stub was sent fewer than {n} requests, so the run stopped somewhere unexpected");
}

/// What every one of these commands owes the conversation.
///
/// `says` is the word the command prints when it worked. Without it a run that refused the
/// switch -- a typo in a model name, a provider that is not there -- would pass this by never
/// making the request the rest of it is about.
///
/// The conversation has to survive in two places, and they are not the same place. What the
/// model is sent is the context; the file is what survives the process. A fix that kept one and
/// not the other would look complete from either end alone -- and the file's half has two ways to
/// be wrong, which is why it is asserted as a *count*: the conversation must be in a file, and in
/// exactly one of them. The run used to seed a second file with the whole conversation copied into
/// it, so both halves of this were "true" while one conversation was two conversations.
fn the_switch_kept_the_conversation(
    text: &str,
    says: &str,
    command: &str,
    files: &[(String, String)],
    bodies: &[String],
) {
    assert!(
        text.contains(says),
        "`{command}` did not switch anything, so this proves nothing: {text:?}"
    );
    assert_eq!(
        bodies.len(),
        2,
        "expected one request before the switch and one after it: {bodies:?}"
    );
    let after = sent_messages(&bodies[1]);
    for needle in [
        "the earlier question",
        "the earlier answer",
        "the question in this run",
        "and now?",
    ] {
        assert!(
            after.contains(needle),
            "`{command}` lost {needle:?} from what the model is sent: {after}"
        );
    }

    // The conversation has to be *in a file*, and in one that holds all of it: a switch that
    // only fixed the request would leave a conversation that a restart cannot recover, and
    // `/resume` is the only way back to one. The file is the one the run was already writing --
    // `/resume 111-1` put it there -- because a switch replaces the agent and not the
    // conversation. `switching_provider_keeps_the_conversation_in_its_file` is the same claim from
    // the page's side, where the symptom was a second row appearing in the sidebar.
    let holders: Vec<&str> = files
        .iter()
        .filter(|(_, text)| text.contains("the earlier question"))
        .map(|(name, _)| name.as_str())
        .collect();
    assert_eq!(
        holders,
        vec!["111-1.jsonl"],
        "`{command}` left the conversation in {} file(s) rather than the one it was in: {files:?}",
        holders.len()
    );
    let written = &files
        .iter()
        .find(|(name, _)| name == "111-1.jsonl")
        .expect("the resumed session")
        .1;
    for needle in [
        "the earlier question",
        "the earlier answer",
        "the question in this run",
        "and now?",
    ] {
        assert!(
            written.contains(needle),
            "the file `{command}` carried the conversation into is missing {needle:?}: 111-1.jsonl"
        );
    }
}

/// `/compact` is one request, one line in the file, and a smaller request after it.
///
/// The three claims are one feature. The summary has to reach the request that follows, or the fold
/// only exists on screen; the messages it folded must *not*, or nothing got smaller; and the file has
/// to say where the fold cut, or tomorrow's run asks all of it again at full price with nothing on
/// screen to explain why. The stub answers every request with the same sentence, which is what makes
/// the second claim checkable: the summary is that sentence, framed, and the folded messages are the
/// thing that has to be gone.
#[tokio::test]
async fn compacting_folds_the_earlier_messages_into_one_line_and_a_smaller_request() {
    let server = MockServer::start().await;
    answer_once(&server).await;
    let config = stub_config(&server.uri());

    let (text, files, bodies, _) =
        through_a_switch(&server, "compact-fold", &config, "/compact").await;

    assert!(
        text.contains("folded"),
        "`/compact` did not fold anything, so this proves nothing: {text:?}"
    );
    assert_eq!(
        bodies.len(),
        3,
        "expected one request per turn plus one for the summary: {bodies:?}"
    );

    // The request the summary was asked for: the messages about to be folded, and **no tools** --
    // nothing a summarizer says may act. The tool schemas are a screenful each, so their absence is
    // also what makes this request the cheap one.
    let asked = sent_messages(&bodies[1]);
    for needle in ["the earlier question", "the earlier answer"] {
        assert!(
            asked.contains(needle),
            "the summary was not asked about {needle:?}: {asked}"
        );
    }
    assert!(
        asked.contains("Summarize the conversation above"),
        "the summary was asked for without saying what kind of answer is wanted: {asked}"
    );
    assert!(
        !bodies[1].contains("\"tools\""),
        "the summary was asked for with tools, which makes it a turn and not a summary: {}",
        bodies[1]
    );

    // The request after it: the summary stands where the folded messages were.
    let after = sent_messages(&bodies[2]);
    assert!(
        after.contains("summarized by flint at the person's request"),
        "the next request did not carry the summary, so the fold was only on screen: {after}"
    );
    for gone in ["the earlier question", "the earlier answer"] {
        assert!(
            !after.contains(gone),
            "`/compact` left {gone:?} in the request it was supposed to shrink: {after}"
        );
    }
    for kept in ["the question in this run", "and now?"] {
        assert!(
            after.contains(kept),
            "`/compact` folded too much -- {kept:?} is gone from the request: {after}"
        );
    }

    // The file: the fold is a line, and everything it folds is still in there. The record got no
    // smaller, which is the promise that makes the request shrinking acceptable.
    let written = &files
        .iter()
        .find(|(name, _)| name == "111-1.jsonl")
        .expect("the resumed session")
        .1;
    assert!(
        written.contains("\"type\":\"compact\""),
        "the fold was not written down, so a resume would send the whole conversation again: {written}"
    );
    for needle in ["the earlier question", "the earlier answer"] {
        assert!(
            written.contains(needle),
            "compacting removed {needle:?} from the conversation's own file: {written}"
        );
    }
}

/// A fold written into a file is what a *resumed* run acts on.
///
/// This is the half that makes it a file feature: the run that compacted is gone by tomorrow, and a
/// resume that ignored the line would send the folded messages again -- the one failure a person
/// could not see coming, because the transcript on screen shows them either way. The pointer here is
/// written by hand and computed the way a person would compute it, by finding the line the fold keeps
/// and counting the bytes before it. That is the promise `docs/session-format.md` makes about editing
/// this line yourself.
#[tokio::test]
async fn a_fold_in_the_file_is_what_a_resumed_run_sends() {
    use std::io::Write;

    let server = MockServer::start().await;
    answer_once(&server).await;
    let home = test_home("compact-resume", &server.uri());
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("working directory");
    let sessions = home.join("sessions");

    let kept = r#"{"type":"chat","message":{"role":"user","content":"the kept question"}}"#;
    let mut body = String::new();
    body.push_str(&meta_line("222-2"));
    body.push('\n');
    body.push_str(r#"{"type":"chat","message":{"role":"user","content":"the folded question"}}"#);
    body.push('\n');
    body.push_str(r#"{"type":"chat","message":{"role":"assistant","content":"the folded answer"}}"#);
    body.push('\n');
    body.push_str(kept);
    body.push('\n');
    let from = body.find(kept).expect("the line the fold keeps");
    body.push_str(&format!(
        r#"{{"type":"compact","summary":"FOLDED SUMMARY","from":{from}}}"#
    ));
    write_session(&sessions, "222-2.jsonl", &[&body], 10);

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
    child
        .stdin
        .as_mut()
        .expect("no stdin handle")
        .write_all(b"/resume 222-2\nand now?\n")
        .expect("failed to write stdin");
    wait_for_requests(&server, 1).await;
    let _ = child.wait_with_output().expect("flint did not finish");
    let bodies: Vec<String> = server
        .received_requests()
        .await
        .expect("requests")
        .iter()
        .map(|request| String::from_utf8_lossy(&request.body).to_string())
        .collect();
    let _ = std::fs::remove_dir_all(&home);

    assert_eq!(
        bodies.len(),
        1,
        "one question should be one request: {bodies:?}"
    );
    let sent = sent_messages(&bodies[0]);
    assert!(
        sent.contains("FOLDED SUMMARY"),
        "a resumed run sent the conversation whole, ignoring the fold in its file: {sent}"
    );
    for gone in ["the folded question", "the folded answer"] {
        assert!(
            !sent.contains(gone),
            "the fold in the file was not applied, so {gone:?} was paid for again: {sent}"
        );
    }
    for kept in ["the kept question", "and now?"] {
        assert!(
            sent.contains(kept),
            "the fold took {kept:?} with it, which is the request being broken rather than smaller: {sent}"
        );
    }
}

/// The messages out of a request body, so a failure reads as a conversation.
///
/// The body also carries every tool schema, which is a screenful per tool and says nothing about
/// what this is asserting.
fn sent_messages(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| value.get("messages").map(|messages| messages.to_string()))
        .unwrap_or_else(|| body.to_string())
}

/// The config `test_home` writes, as a string, for the tests that need to change it.
fn stub_config(uri: &str) -> String {
    format!(
        "default_provider = \"stub\"\n\n\
         [[providers]]\n\
         name = \"stub\"\n\
         base_url = \"{uri}\"\n\
         model = \"stub-model\"\n\
         api_key = \"not-a-real-key\"\n"
    )
}

/// Switching model must not throw the conversation away.
///
/// Measured before it was fixed: `/resume` a file holding a question and an answer, `/model
/// <other>`, then one line -- and the request that went out had two messages, the system
/// prompt and the new line. The command builds a fresh agent around the new model, and a fresh
/// agent has an empty history and a new session file, so everything said before it was gone
/// from the request *and* from the file while the transcript on screen went on showing it.
#[tokio::test]
async fn a_model_switch_keeps_the_conversation() {
    let server = MockServer::start().await;
    answer_once(&server).await;
    let config = format!(
        "default_provider = \"stub\"\n\n\
         [[providers]]\n\
         name = \"stub\"\n\
         base_url = \"{}\"\n\
         model = \"stub-model\"\n\
         models = [\"stub-other\"]\n\
         api_key = \"not-a-real-key\"\n",
        server.uri()
    );

    let (text, files, bodies, _) =
        through_a_switch(&server, "model-switch-keeps", &config, "/model stub-other").await;
    let named = files
        .iter()
        .find(|(_, text)| text.contains(r#""model":"stub-other""#))
        .map(|(name, _)| name.clone());

    the_switch_kept_the_conversation(
        &text,
        "model stub-other",
        "/model stub-other",
        &files,
        &bodies,
    );
    // The file the conversation moved into has to name the model it is being continued with:
    // `Meta` is what `/resume` believes about which model a session was held with.
    assert!(
        named.is_some(),
        "the conversation was carried into a file that names a different model: {files:?}"
    );
}

/// The same guarantee through `/provider`, which is the other way to reach it.
///
/// Both commands end in `switch_provider`, and the test is separate because they do not reach
/// it by the same route: `/provider` also starts and stops local engines, and it is the one the
/// help text points at for changing endpoints.
#[tokio::test]
async fn a_provider_switch_keeps_the_conversation() {
    let server = MockServer::start().await;
    answer_once(&server).await;
    let config = format!(
        "default_provider = \"stub\"\n\n\
         [[providers]]\n\
         name = \"stub\"\n\
         base_url = \"{uri}\"\n\
         model = \"stub-model\"\n\
         api_key = \"not-a-real-key\"\n\n\
         [[providers]]\n\
         name = \"other\"\n\
         base_url = \"{uri}\"\n\
         model = \"stub-model\"\n\
         api_key = \"not-a-real-key\"\n",
        uri = server.uri()
    );

    let (text, files, bodies, _) =
        through_a_switch(&server, "provider-switch-keeps", &config, "/provider other").await;

    the_switch_kept_the_conversation(&text, "switched to other", "/provider other", &files, &bodies);
    assert!(
        files
            .iter()
            .any(|(_, text)| text.contains(r#""provider":"other""#)),
        "the conversation was carried into a file that names a different provider: {files:?}"
    );
}

/// `/config set` is the wizard's one-line form, and what it changes is true of *this* run.
///
/// This is the command the page's form needed and the terminal did not have (`/config set <key>
/// <value>`), and it is the wizard's four questions asked and answered in one line -- so the page can
/// offer a field per setting instead of a row that hands the run to four questions nobody is there to
/// answer.
///
/// The rebuild is why this is an integration test rather than a unit test beside `set_config_key`: a
/// setting assigned into the config is a setting the running run never sees, because every tool holds
/// a *copy* taken when the agent was built, and `max_steps` went into the agent itself. "Saved" without
/// a rebuild is the wizard's old lie -- it printed the new value while the tools used the old one --
/// and what the conversation surviving proves here is that the rebuild happened and produced a working
/// agent. `reload_agent` is the same function `/reload` uses, and `a_reload_keeps_the_conversation`
/// asserts the same thing about that path.
#[tokio::test]
async fn a_setting_changed_by_command_is_in_force_in_this_run() {
    let server = MockServer::start().await;
    answer_once(&server).await;
    let config = stub_config(&server.uri());

    let (text, files, bodies, written) = through_a_switch(
        &server,
        "config-set-in-force",
        &config,
        "/config set max_steps 7",
    )
    .await;

    the_switch_kept_the_conversation(&text, "max_steps = 7", "/config set", &files, &bodies);
    assert!(
        written.contains("max_steps = 7"),
        "/config set reported a change it did not write: {written:?}"
    );
    assert!(
        text.contains("in force now"),
        "the change was made without the run being rebuilt around it, which is the half that makes \
         the sentence true: {text:?}"
    );
}

/// And through `/reload`, which is the one the model is told to use.
///
/// The system prompt tells the model to run `/reload` after editing its own config, so this is
/// the route a conversation is most likely to be destroyed by -- and it destroyed it for no
/// reason at all, since a reload changes nothing about the conversation.
#[tokio::test]
async fn a_reload_keeps_the_conversation() {
    let server = MockServer::start().await;
    answer_once(&server).await;
    let config = stub_config(&server.uri());

    let (text, files, bodies, _) =
        through_a_switch(&server, "reload-keeps", &config, "/reload").await;

    the_switch_kept_the_conversation(&text, "reloaded", "/reload", &files, &bodies);
}

/// The page follows the file the run is writing, through a switch as well.
///
/// Every command that replaces the agent also gives it a session file of its own, and the view
/// has to be told which one: it is the file `/session` serves and the one the feed tails.
/// `/new` and `/resume` said so; `/model` and `/provider` -- the two the page's own pickers
/// reach -- did not, so the view stayed on a file nobody was writing and simply stopped moving.
///
/// `/session` serves the followed file **byte for byte**, which makes the assertion a sharp one:
/// the line typed after the switch is in the new file and in no other.
/// Write one line to a run's stdin, or say why the run could not take it.
///
/// The same lesson the check below was written for, applied one step earlier: this test failed once on
/// the ubuntu runner with `failed to write stdin: Os { code: 32, kind: BrokenPipe }`, which says a pipe
/// closed and nothing about *why the run is gone* -- the two files that would say were written to and
/// never read. A run that is gone cannot answer, so both the status and the two files are carried out.
fn write_to_run(
    child: &mut std::process::Child,
    line: &[u8],
    errors: &std::path::Path,
    log: &std::path::Path,
) {
    use std::io::Write;

    let stdin = child.stdin.as_mut().expect("no stdin handle");
    if let Err(failed) = stdin.write_all(line) {
        let status = child.try_wait().ok().flatten();
        panic!(
            "the run could not be given {line:?}: {failed} (status {status:?})\n  stderr: {}\n  \
             transcript: {}",
            std::fs::read_to_string(errors).unwrap_or_default().trim(),
            std::fs::read_to_string(log).unwrap_or_default().trim()
        );
    }
}

#[tokio::test]
async fn the_view_follows_the_conversation_through_a_switch() {
    let server = MockServer::start().await;
    answer_once(&server).await;
    let home = test_home("view-follows", &server.uri());
    let config = format!(
        "default_provider = \"stub\"\n\n\
         [[providers]]\n\
         name = \"stub\"\n\
         base_url = \"{}\"\n\
         model = \"stub-model\"\n\
         models = [\"stub-other\"]\n\
         api_key = \"not-a-real-key\"\n",
        server.uri()
    );
    std::fs::write(home.join("config.toml"), config).expect("failed to write the test config");
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("working directory");
    write_session(
        &home.join("sessions"),
        "111-1.jsonl",
        &[
            &meta_line("111-1"),
            r#"{"type":"chat","message":{"role":"user","content":"the earlier question"}}"#,
        ],
        10,
    );

    // The transcript goes to a file rather than a pipe: the URL has to be read while the
    // process is still running, and this is where it is printed.
    let log = home.join("transcript.txt");
    // ...and so does its stderr, which used to go to `/dev/null` here and nowhere else in this file.
    // This test failed once on the ubuntu runner with "connect to the view: Connection refused", and
    // the reason was unreadable: a refused connection means the listener is gone -- the URL is printed
    // *after* the socket is bound, and the kernel takes connections into the backlog whether or not
    // the accept task has been scheduled yet -- so the question was what ended the run, and the answer
    // had been thrown away. Kept now, and named in the panic if the run is gone by the time the view
    // is asked to answer.
    let errors = home.join("stderr.txt");
    let mut child = binary()
        .arg("--web")
        .current_dir(&work)
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .stdin(std::process::Stdio::piped())
        .stdout(std::fs::File::create(&log).expect("transcript file"))
        .stderr(std::fs::File::create(&errors).expect("stderr file"))
        .spawn()
        .expect("failed to run flint");

    let (port, token) = port_and_token(&wait_for_url(&log));
    write_to_run(&mut child, b"/resume 111-1\nthe question in this run\n", &errors, &log);
    wait_for_requests(&server, 1).await;
    write_to_run(&mut child, b"/model stub-other\nand now?\n", &errors, &log);
    // The second request means the turn after the switch is under way, so the switch itself has
    // happened and the new file has the line in it.
    wait_for_requests(&server, 2).await;

    // A run that is gone cannot answer, and saying so is worth more than the connection error: this
    // is where the one unreadable failure of this test would have been read.
    if let Ok(Some(status)) = child.try_wait() {
        panic!(
            "the run ended ({status}) before its view could be read\n  stderr: {}\n  transcript: {}",
            std::fs::read_to_string(&errors).unwrap_or_default().trim(),
            std::fs::read_to_string(&log).unwrap_or_default().trim()
        );
    }

    let served = http_get(port, "/session", &token);
    drop(child.stdin.take());
    let exited = wait_for_exit(&mut child, 20);
    let _ = std::fs::remove_dir_all(&home);

    assert!(exited, "flint did not exit");
    assert!(
        served.contains("and now?"),
        "the view is not following the conversation after the switch: {served:?}"
    );
}

/// Wait for the view to print where it is, and return the URL.
fn wait_for_url(log: &std::path::Path) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let text = std::fs::read_to_string(log).unwrap_or_default();
        if let Some(at) = text.find("http://127.0.0.1:") {
            // Taken character by character rather than by splitting on whitespace: the line
            // carries colour codes when the terminal is not asked to drop them.
            let url: String = text[at..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || ":/?=&._-".contains(*c))
                .collect();
            if url.contains("token=") {
                return url;
            }
        }
        if std::time::Instant::now() >= deadline {
            panic!("the view never printed a URL: {text:?}");
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

/// The port and the token out of the URL the view prints.
fn port_and_token(url: &str) -> (u16, String) {
    let after = url
        .strip_prefix("http://127.0.0.1:")
        .unwrap_or_else(|| panic!("not the view's URL: {url}"));
    let (port, rest) = after.split_once('/').unwrap_or_else(|| panic!("no path in {url}"));
    let port: u16 = port.parse().unwrap_or_else(|_| panic!("no port in {url}"));
    let token = rest
        .split_once("token=")
        .unwrap_or_else(|| panic!("no token in {url}"))
        .1
        .to_string();
    (port, token)
}

/// Read a route from a `--web` run, or say why the run could not answer it.
///
/// A refused connection is the one failure a `--web` test cannot read from the response, because there
/// is none: `http_get` names the route and the port, and nothing about *why the run is gone*. On
/// 2026-09-18 the ubuntu runner refused a connection here -- `a_background_command_is_a_job_the_page_can_watch_end`,
/// which sent its stderr to `/dev/null` and so could not say more -- one push after a different `--web`
/// test lost the same race as a broken stdin pipe. Neither has been reproduced, so neither is "fixed";
/// what a repeat needs is the two files that know, which is what this helper exists to hand over. It is
/// the same rule the view-follows test learned for itself: a run that is gone cannot answer, and saying
/// so is worth more than the connection error.
fn get_or_say(
    port: u16,
    route: &str,
    token: &str,
    child: &mut std::process::Child,
    errors: &std::path::Path,
    log: &std::path::Path,
) -> String {
    if let Ok(Some(status)) = child.try_wait() {
        panic!(
            "the run ended ({status}) before {route} could be read\n  stderr: {}\n  transcript: {}",
            std::fs::read_to_string(errors).unwrap_or_default().trim(),
            std::fs::read_to_string(log).unwrap_or_default().trim()
        );
    }
    http_get(port, route, token)
}

/// GET one route from the view and return the body.
///
/// The token goes in `X-Flint-Token` and never in the query string: `/` is the one route that
/// accepts it there, because `/` is the one a person pastes into a browser. A token in a query
/// string travels into `Referer` headers and logs, which is why no other route will read one.
fn http_get(port: u16, route: &str, token: &str) -> String {
    use std::io::{Read, Write};

    // The route and the port are in the message because a refused connection is the one failure here
    // that cannot be read from the response: there is none. Every test in this file that opens a view
    // reaches the same helper, so the address is worth naming once, here.
    let mut sock = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap_or_else(|e| {
        panic!("cannot connect to the view at 127.0.0.1:{port} for {route}: {e}")
    });
    write!(
        sock,
        "GET {route} HTTP/1.1\r\nhost: 127.0.0.1:{port}\r\nX-Flint-Token: {token}\r\n\
         connection: close\r\n\r\n"
    )
    .expect("write the request");
    let mut raw = Vec::new();
    sock.read_to_end(&mut raw).expect("read the response");
    let text = String::from_utf8_lossy(&raw).to_string();
    text.split_once("\r\n\r\n")
        .map(|(_, body)| body.to_string())
        .unwrap_or(text)
}

/// Open a route that never ends -- `/events` -- and leave the socket ready to read frames from.
///
/// Not `http_get`: that reads to the end of the response, and this response has no end. A live
/// feed is read as it arrives, which is also the only way to see the order things arrived in.
fn http_stream(port: u16, route: &str, token: &str) -> std::net::TcpStream {
    use std::io::Write;

    let mut sock = std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect to the view");
    write!(
        sock,
        "GET {route} HTTP/1.1\r\nhost: 127.0.0.1:{port}\r\nX-Flint-Token: {token}\r\n\
         accept: text/event-stream\r\nconnection: keep-alive\r\n\r\n"
    )
    .expect("write the request");
    sock
}

/// Read a live stream until `needle` has been seen, and return everything read so far.
///
/// The deadline is the point: what this waits for is a frame that a broken build never sends, so
/// without one it would hang instead of failing -- and a test that hangs is a test nobody can read
/// the answer from. The read timeout is what lets one thread watch the clock between reads: a
/// timeout means "nothing yet", and a read of zero bytes means the stream ended.
fn read_until(sock: &mut std::net::TcpStream, needle: &str, secs: u64) -> String {
    use std::io::Read;

    sock.set_read_timeout(Some(std::time::Duration::from_millis(200)))
        .expect("set a read timeout");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    let mut seen = String::new();
    let mut chunk = [0u8; 8192];
    while std::time::Instant::now() < deadline {
        match sock.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => seen.push_str(&String::from_utf8_lossy(&chunk[..n])),
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(_) => break,
        }
        if seen.contains(needle) {
            break;
        }
    }
    seen
}

/// POST one line to the running flint's prompt, the way the page's composer does.
///
/// The route and the body shape are the page's (`{"text": ...}`), not the process's stdin: what
/// these tests are about is a page being able to change the thing a frame describes, which is
/// §8's whole shape -- the page composes the terminal's own command line and the REPL decides
/// what it means.
fn post_message(port: u16, token: &str, text: &str) -> String {
    post_to(port, token, "/message", text)
}

/// The same body, to another route: `/report` is a page asking for a listing to read in its own
/// panel. Kept as one function so the two routes cannot drift in how they are framed on the wire --
/// the difference between them is what the *process* does with the line, not how it is sent.
fn post_to(port: u16, token: &str, route: &str, text: &str) -> String {
    use std::io::{Read, Write};

    let body = format!("{{\"text\":{}}}", serde_json::Value::String(text.to_string()));
    let mut sock = std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect to the view");
    write!(
        sock,
        "POST {route} HTTP/1.1\r\nhost: 127.0.0.1:{port}\r\nX-Flint-Token: {token}\r\n\
         content-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
    .expect("write the request");
    let mut raw = Vec::new();
    sock.read_to_end(&mut raw).expect("read the response");
    String::from_utf8_lossy(&raw).to_string()
}

/// A turn that is stopped is over *on the page*, not only in the terminal.
///
/// Reported from a real session: the browser went on showing a half answer as though it were still
/// arriving, and nothing about it ever changed again. The page closes an answer when
/// `message.completed` arrives and an interrupted turn never reaches it -- the future is dropped --
/// so the end of the turn has to be said separately. The button that sends the stop is checked over
/// the page's own bytes in `tests/web_view.rs`; this is the half that needs a real process, because
/// the frame has to come out of a run that was actually interrupted.
#[tokio::test]
async fn a_stopped_turn_tells_the_page_it_is_over() {
    use std::io::Write;

    let provider = HangingProvider::start("A HALF-WRITTEN ARTICLE\n");
    let home = test_home("stop-settles-view", &provider.base_url);
    let log = home.join("transcript.txt");
    // The run's stderr, kept rather than sent to `/dev/null`: a `--web` run that dies mid-test
    // otherwise reports a connection that was refused, or a transcript that simply stops, and
    // nothing about *why* -- the class the flaky jobs test came from, which `HANDOFF.md` names.
    // It sits in this test's own home, and a test that panics leaves the whole home behind.
    let errors = home.join("stderr.txt");
    let mut child = binary()
        .arg("--web")
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .stdin(std::process::Stdio::piped())
        .stdout(std::fs::File::create(&log).expect("transcript file"))
        .stderr(std::fs::File::create(&errors).expect("stderr file"))
        .spawn()
        .expect("failed to run flint");

    let (port, token) = port_and_token(&wait_for_url(&log));
    // Watching *before* the turn starts, so what is read below is the run's own frames and not the
    // snapshot a page gets when it arrives mid-turn.
    let mut events = http_stream(port, "/events", &token);

    let mut stdin = child.stdin.take().expect("no stdin handle");
    stdin
        .write_all(b"write me an article\n")
        .expect("failed to write stdin");
    let drawn = read_until(&mut events, "A HALF-WRITTEN ARTICLE", 20);
    assert!(
        drawn.contains("\"type\":\"message.delta\""),
        "the answer never reached the page, so nothing was interrupted: {drawn:?}"
    );

    stdin.write_all(b"/stop\n").expect("failed to write stdin");
    let settled = read_until(&mut events, "turn.completed", 20);
    drop(stdin);
    let exited = wait_for_exit(&mut child, 20);
    // Read before the directory goes: when this fails, the terminal's own account of the same
    // moment is the other half of what happened.
    let transcript = std::fs::read_to_string(&log).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&home);

    assert!(exited, "flint did not exit");
    assert!(
        settled.contains("\"type\":\"turn.completed\""),
        "the page was never told the stopped turn was over, so its half answer stays open. \
         Frames: {settled:?} Terminal: {transcript:?}"
    );
    // And the status line ends with it. This is the same fact for a run whose stdout is not a
    // terminal, and it was the same failure: `activity_done` cleared the activity only when there
    // was a terminal to draw it on, so the browser was told what the stopped turn had been doing
    // for as long as the page stayed open.
    assert!(
        settled.contains("\"text\":\"\",\"type\":\"status\""),
        "the page's status line still says what the stopped turn was doing. Frames: {settled:?}"
    );
}

/// A report asked for while a turn is running waits for the turn instead of racing it.
///
/// This is the gap `docs/web-mode.md` §11 left open: the wait is stashed (`Handover`, not an
/// interrupt), and asserting it needs a turn slow enough to ask during -- which this provider is,
/// because it draws a delta and then holds the socket open.
///
/// The *order* is the claim, so it is read off one feed rather than inferred from a clock: the
/// report's answer has to arrive after `turn.completed`, since a report is a read and a read that
/// raced the turn would be a second writer in the transcript. Three facts, so that passing cannot
/// mean the route quietly dropped it: the route accepted it, the turn ended (a real `/stop`, the
/// ending a person can cause), and the answer came back after that ending.
#[tokio::test]
async fn a_report_asked_for_mid_turn_waits_for_the_turn() {
    use std::io::Write;

    let provider = HangingProvider::start("A HALF-WRITTEN ARTICLE\n");
    let home = test_home("report-mid-turn", &provider.base_url);
    let log = home.join("transcript.txt");
    // The run's stderr, kept rather than sent to `/dev/null`: a `--web` run that dies mid-test
    // otherwise reports a connection that was refused, or a transcript that simply stops, and
    // nothing about *why* -- the class the flaky jobs test came from, which `HANDOFF.md` names.
    // It sits in this test's own home, and a test that panics leaves the whole home behind.
    let errors = home.join("stderr.txt");
    let mut child = binary()
        .arg("--web")
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .stdin(std::process::Stdio::piped())
        .stdout(std::fs::File::create(&log).expect("transcript file"))
        .stderr(std::fs::File::create(&errors).expect("stderr file"))
        .spawn()
        .expect("failed to run flint");

    let (port, token) = port_and_token(&wait_for_url(&log));
    let mut events = http_stream(port, "/events", &token);

    let mut stdin = child.stdin.take().expect("no stdin handle");
    stdin
        .write_all(b"write me an article\n")
        .expect("failed to write stdin");
    // In flight once its first words have reached the page -- the same moment the other mid-turn
    // tests use, and the only moment a report can be asked for *during* something.
    let drawn = read_until(&mut events, "A HALF-WRITTEN ARTICLE", 20);
    assert!(
        drawn.contains("\"type\":\"message.delta\""),
        "the turn never started, so there was nothing to wait for: {drawn:?}"
    );

    // The report, through the route the panel uses, while the model is still talking.
    let asked = post_to(port, &token, "/report", "/config");
    assert!(
        asked.starts_with("HTTP/1.1 202"),
        "the report route refused a read it should have taken: {asked:?}"
    );

    stdin.write_all(b"/stop\n").expect("failed to write stdin");
    // Read past the turn's own frame to the answer: both have to be in this one slice for the order
    // between them to mean anything.
    let settled = read_until(&mut events, "\"input\":\"/config\"", 20);
    drop(stdin);
    let exited = wait_for_exit(&mut child, 20);
    let transcript = std::fs::read_to_string(&log).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&home);

    assert!(exited, "flint did not exit: {transcript}");
    let ended = settled
        .find("\"type\":\"turn.completed\"")
        .unwrap_or_else(|| panic!("the stopped turn was never said to be over: {settled:?}"));
    let answered = settled
        .find("\"input\":\"/config\"")
        .expect("the report's answer is not in the slice that was read");
    assert!(
        ended < answered,
        "the report was answered before the turn was over, so the read raced it: {settled:?}"
    );
    // And it came back with something to read, because a panel that answers with nothing is
    // indistinguishable from a press that never arrived.
    assert!(
        settled.contains("\"panel\":true") && settled.contains("config.toml"),
        "the report was answered with nothing to put in the panel: {settled:?}"
    );
}

/// `readonly` guarded one door out of three, and the other two were the person's own.
///
/// `docs/features.md` §2.3 says `flint exec` and the `exec` **tool** "go through the same readonly
/// judgement"; they did not -- `exec_is_readonly` had exactly one caller, the tool -- and the banner
/// this run prints on the same screen says "writes and mutating commands are refused" while `!cmd`
/// typed at its prompt created the file anyway (measured). A guard that one door walks around is not
/// a guard, and here the thing being falsified is flint's own sentence about itself.
///
/// Both halves are asserted in one run, because the fix must not turn `!` off: an inspection command
/// still runs while the run is read-only, which is the whole of what "useful for a first look around
/// an unfamiliar machine" asks for. The mutating half is a redirect (`echo hi > f`), which is a
/// shell's own way of writing a file and is disqualified by the string rules whatever the platform.
#[test]
fn a_readonly_run_refuses_the_line_the_person_types() {
    let home = test_home("readonly-bang", "http://127.0.0.1:9/v1");
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("working directory");
    let text = repl_of_with(
        &home,
        &work,
        &["--readonly"],
        &["!echo hi > typed-out.txt", "!echo inspected"],
    );
    let created = work.join("typed-out.txt").exists();
    let _ = std::fs::remove_dir_all(&home);

    assert!(
        !created,
        "a readonly run wrote a file through the line the person typed, which the banner on the same \
         screen says is refused:\n{text}"
    );
    assert!(
        text.contains("readonly") && text.to_lowercase().contains("refus"),
        "nothing in the transcript says why the line did not run:\n{text}"
    );
    assert!(
        text.contains("inspected"),
        "`!echo` was refused as well, so `!` is not usable in a readonly run at all -- the refusal \
         has to be the judgement, not the door:\n{text}"
    );
}

/// The `exec` subcommand is the model's `exec` tool by another door, and §2.3 says so out loud.
///
/// It reads a config that exists (`load_existing`) and takes `--readonly` on its own command line, so
/// both ways of asking for the guard were available and neither was consulted: `flint --readonly exec
/// "echo hi > f"` wrote the file and exited 0. What is asserted here is the pair the sentence in
/// `docs/features.md` promises -- the refusal, its exit code, and the inspection command that still
/// has to work -- because a guard that refuses everything is a different bug with the same test.
#[test]
fn flint_exec_honours_the_readonly_flag() {
    let home = test_home("readonly-exec", "http://127.0.0.1:9/v1");
    let work = home.join("work");
    std::fs::create_dir_all(&work).expect("working directory");

    let refused = binary()
        .args(["--readonly", "exec", "echo hi > exec-out.txt"])
        .current_dir(&work)
        .env("FLINT_HOME", &home)
        .output()
        .expect("failed to run flint");
    let created = work.join("exec-out.txt").exists();

    let allowed = binary()
        .args(["--readonly", "exec", "echo inspected"])
        .current_dir(&work)
        .env("FLINT_HOME", &home)
        .output()
        .expect("failed to run flint");
    let _ = std::fs::remove_dir_all(&home);

    let stderr = String::from_utf8_lossy(&refused.stderr).to_string();
    assert!(
        !created,
        "`flint --readonly exec` wrote the file anyway, which is the flag being ignored: stdout {} \
         stderr {}",
        String::from_utf8_lossy(&refused.stdout),
        stderr
    );
    assert_eq!(
        refused.status.code(),
        Some(2),
        "a refused invocation is a usage refusal like the others in §2.4, not the child's code: \
         stderr {stderr}"
    );
    assert!(
        stderr.contains("readonly"),
        "the refusal does not say which guard refused it: {stderr}"
    );
    assert_eq!(
        allowed.status.code(),
        Some(0),
        "`exec echo` was refused in a readonly run, so the judgement is on the door rather than on \
         the command: stderr {}",
        String::from_utf8_lossy(&allowed.stderr)
    );
    assert!(
        String::from_utf8_lossy(&allowed.stdout).contains("inspected"),
        "the allowed command did not run: {}",
        String::from_utf8_lossy(&allowed.stdout)
    );
}

/// Asking a conversation that has not been written yet for its name is not an error.
///
/// A file is created by the first thing said in it, so a run that has said nothing has no file to
/// scan -- and asking anyway put the operating system's own sentence on the screen, localized and in
/// the middle of an English transcript:
///
/// ```text
/// /name: cannot stat C:\Users\…\1789718594-50-27488.jsonl: <the OS's own words> (os error 3)
/// ```
///
/// The localized half of that sentence is *described* here rather than quoted, because one of the
/// characters in it is a marker `the_source_tree_contains_no_mojibake` looks for -- it is one of the
/// shapes a CP936 mis-decode leaves behind, so a source file that contains it is reported whatever
/// the reason. The assertion below is the same rule from the other side: none of the OS's phrasing may
/// appear at the prompt, in any language.
///
/// `docs/features.md` §5.5 promises `name: <title>` or `(unnamed)`, and `(unnamed)` is the word this
/// state has: there is no title because there is no conversation yet, which is also what `--no-session`
/// means and why the arm above it has its own sentence. The OS's error is not a report about the
/// conversation, and its language is decided by the machine rather than by flint.
#[test]
fn naming_an_empty_conversation_says_it_is_unnamed() {
    let home = test_home("name-empty", "http://127.0.0.1:9/v1");
    // Nothing said first: this is the run before its first question, which is the state that broke.
    let text = repl(&home, &["/name"]);
    let _ = std::fs::remove_dir_all(&home);

    assert!(
        text.contains("name: (unnamed)"),
        "a conversation with nothing in it did not report itself as unnamed:\n{text}"
    );
    assert!(
        !text.contains("os error") && !text.to_lowercase().contains("cannot stat"),
        "the operating system's own error was printed at the prompt instead of an answer:\n{text}"
    );
}

/// `/stop` with nothing running is an answer, not an unknown command.
///
/// Mid-turn the line never arrives here -- `run_turn`'s own poll loop takes it, deliberately, so that
/// a stop is not followed by the dispatcher saying "nothing is running" about the turn it just ended.
/// Idle, though, it fell through to the fallback, so a command `/help` lists answered
/// `unknown command '/stop'` -- the single most confusing thing a table-driven dispatcher can say
/// about one of its own rows. The turn really is not running, and this is the moment to say so.
#[test]
fn stopping_nothing_says_that_rather_than_unknown_command() {
    let home = test_home("stop-idle", "http://127.0.0.1:9/v1");
    let text = repl(&home, &["/stop"]);
    let _ = std::fs::remove_dir_all(&home);

    assert!(
        text.contains("nothing is running"),
        "`/stop` at an idle prompt did not say what the state is:\n{text}"
    );
    assert!(
        !text.contains("unknown command"),
        "a command `/help` lists was called unknown:\n{text}"
    );
}

/// `/readonly` is the only switch this tool has, so it has to be a switch.
///
/// Measured before it was fixed: `/readonly on` printed "no writes, no mutating commands", the
/// `/config` line printed under it said `readonly = false`, and the file had no `readonly` key at
/// all. The arm worked out the value it meant to set, printed it, and set nothing -- so the guard
/// that flint's own manual calls its only permission control had never been on, and a session that
/// believed it was guarded had full permissions. That is worse than a command that does nothing:
/// it reports a permission the session does not have.
///
/// Both halves are asserted, because they are two different promises: the run in progress, whose
/// tool set is built with the flag, and the file, which is what a later run starts from.
#[test]
fn readonly_guards_the_run_it_is_typed_into() {
    let home = test_home("readonly-on", "http://127.0.0.1:9/v1");
    let text = repl(&home, &["/readonly on", "/config"]);
    let file = std::fs::read_to_string(home.join("config.toml")).expect("the config file");
    // A second process, because what has to survive is the file: nothing else carries across.
    let next_run = repl(&home, &["/config"]);
    let _ = std::fs::remove_dir_all(&home);

    assert!(
        file.contains("readonly = true"),
        "the config file was not changed, so the setting does not outlive the run: {file}"
    );

    // The line `/config` prints is the value *in force*: the agent's, not the file's -- see the
    // comment there. A parenthetical would mean the two disagree, which is the bug in miniature.
    let in_force = text
        .lines()
        .find(|line| line.contains("readonly") && line.contains('='))
        .unwrap_or("")
        .trim()
        .to_string();
    assert!(
        in_force.ends_with("= true"),
        "the run that typed it is not guarded, and says so: {in_force:?} (whole transcript: {text})"
    );

    let in_force = next_run
        .lines()
        .find(|line| line.contains("readonly") && line.contains('='))
        .unwrap_or("")
        .trim()
        .to_string();
    assert!(
        in_force.ends_with("= true"),
        "the next run is not guarded: {in_force:?} (whole transcript: {next_run})"
    );
}

/// `/verbose off` is a setting, and a setting has to survive the file.
///
/// It did not. The arm saved `cfg.verbose = next >= CHATTY`, which is a `bool` that cannot say
/// "off": `false` was both the quietest setting and the default, so `/verbose off` wrote `false`
/// and the next run started *on*. `/config` was worse than unhelpful about it -- it printed
/// `verbose = false` while the run was printing a line per tool call, so the report agreed with
/// the file and both disagreed with the run.
///
/// Three promises here, and they are the three ways a setting can fail to be one: the file holds
/// the word, the run that typed it is quiet, and a second process starts quiet. The page's switch
/// is drawn from the same three-valued name, which is why it cannot be built on a bool.
#[test]
fn verbose_off_is_what_the_file_records() {
    let home = test_home("verbose-off", "http://127.0.0.1:9/v1");
    let text = repl(&home, &["/verbose off", "/config"]);
    let file = std::fs::read_to_string(home.join("config.toml")).expect("the config file");
    // A second process, because what has to survive is the file: nothing else carries across.
    let next_run = repl(&home, &["/config"]);
    let _ = std::fs::remove_dir_all(&home);

    assert!(
        file.contains("verbose = \"off\""),
        "the file does not record the setting, so it does not outlive the run: {file}"
    );
    assert!(
        text.contains("verbose           = off"),
        "the run that typed it does not report itself quiet: {text}"
    );
    assert!(
        next_run.contains("verbose           = off"),
        "the next run does not start quiet: {next_run}"
    );
}

/// A `verbose = true`/`false` in an existing file still means what it always meant.
///
/// `false` printed one line per tool call, which is `on` and not `off`; reading it as `off` would
/// turn every existing config into a silent one. `true` was the level above it, which is `full`.
/// The file is rewritten with the word on the next save, so the two spellings cannot drift apart
/// again -- but they do have to agree for as long as both exist.
#[test]
fn the_old_bool_for_verbose_still_reads_as_it_did() {
    let home = test_home("verbose-legacy", "http://127.0.0.1:9/v1");
    let config = home.join("config.toml");
    let written = std::fs::read_to_string(&config).expect("the test config");

    for (old, word) in [("false", "on"), ("true", "full")] {
        // Prepended, not appended: a bare key after `[[providers]]` belongs to the *provider*
        // table, where it is not a setting at all -- and the first version of this test passed
        // for that reason rather than because the old bool had been read.
        std::fs::write(&config, format!("verbose = {old}\n{written}")).expect("write the old form");
        let shown = repl(&home, &["/config"]);
        assert!(
            shown.contains(&format!("verbose           = {word}")),
            "`verbose = {old}` no longer reads as `{word}`: {shown}"
        );
    }

    // And a word that names nothing is refused rather than silently defaulted: this file is meant
    // to be hand-edited, so a typo has to be reported where it is read.
    std::fs::write(&config, format!("verbose = \"loud\"\n{written}")).expect("write the typo");
    let out = binary()
        .arg("--list-sessions")
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .output()
        .expect("failed to run flint");
    let said = String::from_utf8_lossy(&out.stderr).to_string();
    let _ = std::fs::remove_dir_all(&home);
    assert!(
        said.contains("loud"),
        "a misspelled setting was accepted in silence: {said:?}"
    );
}

/// A command that fails is an answer, not the end of the session.
///
/// Found by driving it: `/verbose loud` — one word wrong — printed
/// `flint: error: expected on|off|full, got 'loud'` and **exited**, taking the conversation with
/// it. The command arms return `Result`, and the REPL loop handed that error out of `interactive`
/// with `?`, where it became the process's exit status. So every command a person can mistype is a
/// way to lose a session — and the page makes it worse rather than better: its composer sends
/// lines to the same place, so a mistyped command typed in the browser ends the run the page is
/// watching, with no way to say why.
///
/// The line the reader refuses already had the right treatment two branches above — print it and
/// carry on — and this is the same answer for the same reason.
#[test]
fn a_command_that_fails_does_not_end_the_session() {
    let home = test_home("bad-command", "http://127.0.0.1:9/v1");
    let text = repl(&home, &["/verbose loud", "/config"]);
    let _ = std::fs::remove_dir_all(&home);

    assert!(
        text.contains("expected on|off|full"),
        "the command's failure was not reported on the terminal: {text}"
    );
    assert!(
        text.contains("verbose           = "),
        "the session ended on a mistyped command instead of answering it: {text}"
    );
}

/// The reasoning level belongs to the run, so a conversation opened afterwards starts at it.
///
/// The level is held on the provider rather than in a local, which is what makes it survive the
/// rebuilds that *keep* a conversation (`/model`, `/provider`, `/reload`). A conversation opened
/// after one of those is still this run, and the level is a decision about how this run asks -- so
/// opening another conversation must not look the level up in the config again. Reported directly,
/// 2026-09-23, one press after the conversation the run was in was deleted: the settings screen said
/// `off`, because the fresh conversation had been built without the level the run was at.
///
/// The level is set *by the command* and not in the file, which is what makes this test able to fail:
/// a config that already said `high` and a run carrying `high` are the same frame, so a test written
/// that way passes whether the level is carried or re-read. What a person does is press the word in
/// the settings screen, which is this.
#[tokio::test]
async fn a_new_conversation_keeps_the_reasoning_level_the_run_was_at() {
    // `test_home`'s file names no level, so the run opens at the default and the setting is the
    // person's rather than the file's -- which is the whole subject here.
    let home = test_home("thinking-new", "http://127.0.0.1:9/v1");

    let log = home.join("transcript.txt");
    let errors = home.join("stderr.txt");
    let mut child = binary()
        .arg("--web")
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .stdin(std::process::Stdio::piped())
        .stdout(std::fs::File::create(&log).expect("transcript file"))
        .stderr(std::fs::File::create(&errors).expect("stderr file"))
        .spawn()
        .expect("failed to run flint");

    let (port, token) = port_and_token(&wait_for_url(&log));
    let mut watching = http_stream(port, "/events", &token);
    // The value rather than the key: `"value":"off"` is the thinking setting's own value and nothing
    // else in the frame spells it -- the ladder itself is offered as a list of choices.
    let opening = read_until(&mut watching, "\"value\":\"off\"", 20);

    let set = post_message(port, &token, "/thinking high");
    let raised = read_until(&mut watching, "\"value\":\"high\"", 20);

    let started = post_message(port, &token, "/new");
    // A second command whose *printed answer* is the fence. The loop pushes the state frame at the top
    // of every turn, so once the terminal has answered this one, the frame `/new` produced exists --
    // and a page opening its settings screen now is handed that frame rather than the one from before
    // the move. The already-open stream cannot carry this claim: `Live::state` drops a frame identical
    // to the last, so a level that was *kept* says nothing there, which is the same silence as a bug
    // that keeps quiet. What a person does is open the screen afterwards, which is this read.
    let fence = post_message(port, &token, "/thinking");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut transcript = String::new();
    while std::time::Instant::now() < deadline {
        transcript = std::fs::read_to_string(&log).unwrap_or_default();
        // Two reports and the line about the move: the level was set, and the run is in a conversation
        // that began after it. Both are the terminal's own words rather than an inference.
        if transcript.matches("thinking ").count() >= 2 && transcript.contains("started a new session") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let mut after = http_stream(port, "/events", &token);
    let kept = read_until(&mut after, "\"key\":\"thinking\"", 20);

    drop(after);
    drop(watching);
    drop(child.stdin.take());
    let exited = wait_for_exit(&mut child, 20);
    let _ = std::fs::remove_dir_all(&home);

    assert!(exited, "flint did not exit");
    assert!(
        opening.contains("\"value\":\"off\""),
        "the run did not open at the default level, so the claim below has no subject: {opening:?}"
    );
    assert!(
        set.starts_with("HTTP/1.1 202") && raised.contains("\"value\":\"high\""),
        "the level was not set, so a conversation opened afterwards could not have kept it: \
         {set:?} {raised:?}"
    );
    assert!(
        started.starts_with("HTTP/1.1 202") && fence.starts_with("HTTP/1.1 202"),
        "a command this test needs to have happened was refused: {started:?} {fence:?}"
    );
    assert!(
        transcript.matches("thinking ").count() >= 2 && transcript.contains("started a new session"),
        "the run did not open a new conversation, so nothing was carried or dropped: {transcript:?}"
    );
    // The report the fence asked for, which is the same fact as the frame below and needs no timing:
    // `started a new session` came before a `thinking` line that still names the level.
    let after_the_move = transcript
        .split_once("started a new session")
        .map(|(_, rest)| rest.to_string())
        .unwrap_or_default();
    assert!(
        after_the_move.contains("thinking high"),
        "the run's own report says it is no longer at the level that was set: {after_the_move:?}"
    );
    assert!(
        kept.contains("\"value\":\"high\""),
        "a conversation opened after the one the level was set in came back at the provider's default \
         instead of the run's: a person who sets a level and opens another conversation loses it, and \
         the settings screen is where they see that. Frame: {kept:?}"
    );
}

/// The level a person sets is kept where the next run reads it, as the other switches on that screen are.
///
/// `/verbose`, `/detail` and `/readonly` write the config; `/thinking` wrote only the conversation's
/// file, so the level somebody chose in the settings dialog was gone from the next run -- and the
/// settings screen, which is where they set it, opened on `off`. Reported directly, 2026-09-23. Both
/// places are written on purpose: the conversation's own line is what a *resumed* conversation comes
/// back at (`resolve_thinking`), and the config key is what the next conversation starts at, which is
/// the rule the other switches already follow. `/hear-peers` is the one that does not persist, and it
/// says so out loud -- this arm said nothing, which is what made the loss look like a bug in the screen.
#[test]
fn the_reasoning_level_a_person_sets_is_kept_for_the_next_run() {
    // `test_home`'s file names no level, so this is a run at the default: the state the complaint
    // describes, and the one a person who has just set a level is looking at afterwards.
    let home = test_home("thinking-kept", "http://127.0.0.1:9/v1");

    let first = repl(&home, &["/thinking high", "/exit"]);
    let written = std::fs::read_to_string(home.join("config.toml")).unwrap_or_default();
    // A second process in the same home, which is the only thing that says what the *next* run does:
    // `/thinking` with no argument reports the level in force, and a run that keeps no conversation of
    // its own comes back at the config's.
    let next = repl(&home, &["/thinking", "/exit"]);
    let _ = std::fs::remove_dir_all(&home);

    assert!(
        first.contains("thinking high"),
        "the level was not set in the run that was asked for it, so nothing was kept either: {first:?}"
    );
    assert!(
        written.contains("thinking = \"high\""),
        "the level never reached the file the next run reads: {written:?}"
    );
    assert!(
        next.contains("thinking high"),
        "the next run came back at the config's word instead of the level that was set -- which is \
         what a person sees as the settings screen turning reasoning off by itself: {next:?}"
    );
}

/// The page is told what its controls could offer, and told again when that changes.
///
/// §8's read channel, and it comes before any control because a picker cannot be drawn without
/// knowing the options -- and because the page must not read `config.toml` for them: a second
/// reader of the same state is a second thing that can disagree with the process. The frame is
/// *state* rather than history, like `status`, so it is also what a page opening at any moment is
/// handed. That is the half a cursor cannot cover: a client with no `last` is not replayed the
/// ring at all, so a run that announced its state before this page loaded would otherwise say
/// nothing for the rest of the session.
///
/// The second half is the write channel composing with it: the line a picker sends goes to
/// `POST /message`, which is the route the composer uses, and what comes back on the feed
/// describes the run that command made.
#[tokio::test]
async fn the_page_is_told_the_state_its_controls_would_show() {
    // No stub server: `/model` rebuilds the agent and saves the config, and no turn is ever run,
    // so nothing here talks to a provider. Two models, because a picker with one option would
    // pass this test while being useless.
    let home = test_home("state-frame", "http://127.0.0.1:9/v1");
    // The reasoning level is set in the file rather than left at its default, because that is the
    // case the page got wrong: a run whose *preset* is `medium` opened its settings screen on `off`.
    // The field the level goes in is set too, or the run would be at `medium` while its requests
    // carried nothing -- a different complaint, and one `/thinking` says out loud.
    std::fs::write(
        home.join("config.toml"),
        "default_provider = \"stub\"\n\n\
         thinking = \"medium\"\n\n\
         [[providers]]\n\
         name = \"stub\"\n\
         base_url = \"http://127.0.0.1:9/v1\"\n\
         model = \"stub-model\"\n\
         models = [\"stub-other\"]\n\
         api_key = \"not-a-real-key\"\n\
         thinking_field = \"reasoning_effort\"\n",
    )
    .expect("the test config");

    let log = home.join("transcript.txt");
    // The run's stderr, kept rather than sent to `/dev/null`: a `--web` run that dies mid-test
    // otherwise reports a connection that was refused, or a transcript that simply stops, and
    // nothing about *why* -- the class the flaky jobs test came from, which `HANDOFF.md` names.
    // It sits in this test's own home, and a test that panics leaves the whole home behind.
    let errors = home.join("stderr.txt");
    let mut child = binary()
        .arg("--web")
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .stdin(std::process::Stdio::piped())
        .stdout(std::fs::File::create(&log).expect("transcript file"))
        .stderr(std::fs::File::create(&errors).expect("stderr file"))
        .spawn()
        .expect("failed to run flint");

    let (port, token) = port_and_token(&wait_for_url(&log));
    let mut watching = http_stream(port, "/events", &token);
    // Read to the *last* field of the frame rather than to its first: a read that stopped at
    // `"type":"state"` would be asserting on the fields behind it -- which is a test that passes
    // or fails on where the kernel happened to split the segment.
    let opening = read_until(&mut watching, "\"models\":[\"stub-model\",\"stub-other\"]", 20);

    let answered = post_message(port, &token, "/model stub-other");
    let changed = read_until(&mut watching, "\"model\":\"stub-other\"", 20);

    // A switch, over the same route: the page sends `/<key> <value>` -- `/verbose full` here --
    // and what it is told afterwards is the value in force. The four things a control needs are the
    // key, the words that key takes, the value it is on, and which screen it belongs on, and the page
    // carries none of them: it is handed all four, which is what keeps a switch from offering a word
    // the command refuses.
    let switched = post_message(port, &token, "/verbose full");
    // The value, not the whole object: a JSON object's key order is serde's business, and a test
    // that pinned it would fail on a change that means nothing to the page.
    let told = read_until(&mut watching, "\"value\":\"full\"", 20);

    // A switch whose values are a ladder rather than a pair, and the only one that changes what a
    // *request* carries rather than what flint does with an answer: `/thinking high`. It goes through
    // the same route, and what comes back is the same three fields -- so a page offers exactly the
    // words the command accepts without carrying a list of them.
    let reasoned = post_message(port, &token, "/thinking high");
    let reasoning = read_until(&mut watching, "\"value\":\"high\"", 20);

    // `--hear-peers` is a decision about *this run*, not about this conversation, so the rebuild has to
    // keep it -- and the frame is where a person reads it back.
    let heard = post_message(port, &token, "/hear-peers on");
    let hearing = read_until(&mut watching, "\"key\":\"hear-peers\"", 20);

    // `/reload` is the third door onto the same rebuild, and the one a person uses after editing the
    // config by hand: it re-reads the file and replaces the agent. What it must not do is quietly turn
    // the reasoning back off under a run that asked for it -- the level in force here is the `high`
    // that `/thinking` set. Read from a *new* subscriber, because that is both how a page that opens
    // its settings screen late gets the state and the only read that is certain: `Live::state` drops a
    // frame identical to the last one, so a reload that changed nothing says nothing, by design.
    let reloaded = post_message(port, &token, "/reload");
    let mut reopened = http_stream(port, "/events", &token);
    let after_reload = read_until(&mut reopened, "\"key\":\"hear-peers\"", 20);

    // And a page that opens *now* -- a second subscriber with no backlog at all, so what it is
    // sent can only be the snapshot.
    let mut later_page = http_stream(port, "/events", &token);
    let late = read_until(&mut later_page, "\"model\":\"stub-other\"", 20);

    drop(later_page);
    drop(watching);
    drop(child.stdin.take());
    let exited = wait_for_exit(&mut child, 20);
    let transcript = std::fs::read_to_string(&log).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&home);

    assert!(exited, "flint did not exit");
    // A failure message about a setting quotes the settings and not the whole frame: the frame carries
    // every command's description and is several kilobytes, and the row a person needs to read is one
    // object at the end of it.
    let only_settings = |frame: &str| -> String {
        frame.rsplit("\"settings\":").next().unwrap_or(frame).to_string()
    };
    assert!(
        opening.contains("\"type\":\"state\""),
        "the page was never told the state, so it has no options to draw. Frames: {opening:?} \
         Terminal: {transcript:?}"
    );
    assert!(
        opening.contains("\"provider\":\"stub\"") && opening.contains("\"model\":\"stub-model\""),
        "the state does not name the provider and model in force: {opening:?}"
    );
    assert!(
        opening.contains("\"models\":[\"stub-model\",\"stub-other\"]"),
        "the state does not carry the models on offer, which is what a picker is drawn from: \
         {opening:?}"
    );
    assert!(
        answered.starts_with("HTTP/1.1 202"),
        "the line the picker sends was not accepted: {answered:?}"
    );
    assert!(
        changed.contains("\"model\":\"stub-other\""),
        "the page was not told the state changed after its own command: {changed:?}"
    );
    // The level the *file* asked for has to survive the switch. `/model` rebuilds the agent, and a
    // person who set themselves a preset and then picked another model must not find their reasoning
    // quietly turned off -- which, on the page, is a settings screen reading `off` while the run was
    // at `medium` a moment before. Every frame carries the settings, so this is asked of the frame
    // the switch itself produced.
    assert!(
        changed.contains("\"key\":\"thinking\"") && changed.contains("\"value\":\"medium\""),
        "the reasoning level was lost by a model switch, so the page draws a run that is not the one \
         running: {said}", said = only_settings(&changed)
    );
    assert!(
        opening.contains("\"key\":\"verbose\"")
            && opening.contains("\"choices\":[\"off\",\"on\",\"full\"]")
            && opening.contains("\"group\":\"run\"")
            && opening.contains("\"kind\":\"select\""),
        "the state does not carry a setting as a key, the words that key takes, and the screen it \
         belongs on, which is what a control is drawn from and what the page must not carry a copy \
         of: {opening:?}"
    );
    // One channel for the switches, and it is this one: a `toggles` field derived from `settings`
    // lived here for exactly one commit, so that the page's old controls kept working while the new
    // ones were built. Both the field and that assertion went with the last reader, and this is what
    // says so -- a frame that grew a second copy of the same switch would be one fact in two places,
    // which is the mistake the switch round existed to remove.
    assert!(
        !opening.contains("\"toggles\""),
        "the state carries a second list of switches beside the settings they are derived from: \
         {opening:?}"
    );
    assert!(
        opening.contains("\"key\":\"thinking\"")
            && opening.contains("\"choices\":[\"off\",\"low\",\"medium\",\"high\"]")
            && opening.contains("\"value\":\"medium\""),
        "the state does not offer the reasoning ladder, or it opens on a level that is not the one \
         in force: a page whose settings screen says `off` while the run is asking for `medium` is \
         reporting a run that does not exist. {said}", said = only_settings(&opening)
    );
    assert!(
        reasoned.starts_with("HTTP/1.1 202"),
        "the line the reasoning switch sends was not accepted: {reasoned:?}"
    );
    assert!(
        reasoning.contains("\"key\":\"thinking\"") && reasoning.contains("\"value\":\"high\""),
        "the run was not shown the reasoning level it had just set: {reasoning:?}"
    );
    assert!(
        heard.starts_with("HTTP/1.1 202") && reloaded.starts_with("HTTP/1.1 202"),
        "a line this test needs to have happened was refused: {heard:?} {reloaded:?}"
    );
    assert!(
        hearing.contains("\"key\":\"hear-peers\"") && hearing.contains("\"value\":\"on\""),
        "the run was not shown the peer relay it had just turned on: {hearing:?}"
    );
    assert!(
        after_reload.contains("\"key\":\"hear-peers\"") && after_reload.contains("\"value\":\"on\""),
        "`/reload` turned the peer relay back off, so the person who asked to hear peers has silently \
         stopped hearing them and the page's own row agrees with the mistake: {said}", said = only_settings(&after_reload)
    );
    assert!(
        after_reload.contains("\"key\":\"thinking\"") && after_reload.contains("\"value\":\"high\""),
        "`/reload` dropped the reasoning level -- the same fault as the model switch, by the third \
         door onto that rebuild: {said}", said = only_settings(&after_reload)
    );
    assert!(
        transcript.contains("thinking high"),
        "the terminal does not say which level is in force, which is the only place the field it \
         goes in is named: {transcript:?}"
    );
    assert!(
        switched.starts_with("HTTP/1.1 202"),
        "the line a switch sends was not accepted: {switched:?}"
    );
    assert!(
        told.contains("\"key\":\"verbose\"") && told.contains("\"value\":\"full\""),
        "a switch was not told the value it had just set: {told:?}"
    );
    assert!(
        late.contains("\"type\":\"state\"") && late.contains("\"model\":\"stub-other\""),
        "a page opening after the change is not handed where things are: {late:?}"
    );
}

/// Switching provider keeps the conversation in the file it is already in.
///
/// Reported from the page: picking another provider in the header "automatically creates a new
/// session", and it did. `continue_conversation` seeded a **new** file with the whole conversation in
/// it, because `Meta` names the provider and model and `--resume` believes it. The result was one
/// conversation in two files with the same messages: the sidebar grew a row nobody asked for, the
/// list numbered the same conversation twice, `/delete` on one of them left the other, and resuming
/// either half resumed half a conversation.
///
/// The fix is an event rather than a file, which is the argument `Usage` already makes for its own
/// numbers: the file is append-only, so what changed is a line in it. `/new` and `/resume` still move
/// to another file, because that is what they are *for*; `/model`, `/provider` and `/reload` replace
/// the agent around a conversation that stays put.
///
/// The question comes first, and it has to: a run that has said nothing is not a conversation, so a
/// switch in one leaves no file at all -- the empty sessions
/// `a_run_level_decision_before_the_first_word_leaves_no_session` is about. Reading the file before the
/// switch is also what makes the "appended to, not rewritten" assertion mean what it says; it used to be
/// read after it, from both sides of the same moment.
#[tokio::test]
async fn switching_provider_keeps_the_conversation_in_its_file() {
    let server = MockServer::start().await;
    answer_once(&server).await;
    let home = test_home("switch-file", &server.uri());
    std::fs::write(
        home.join("config.toml"),
        format!(
            "default_provider = \"stub\"\n\n\
             [[providers]]\n\
             name = \"stub\"\n\
             base_url = \"{}\"\n\
             model = \"stub-model\"\n\
             api_key = \"not-a-real-key\"\n\n\
             [[providers]]\n\
             name = \"other\"\n\
             base_url = \"{}\"\n\
             model = \"other-model\"\n\
             api_key = \"not-a-real-key\"\n",
            server.uri(),
            server.uri()
        ),
    )
    .expect("a config with two providers");
    let sessions = home.join("sessions");
    let log = home.join("transcript.txt");
    let mut child = binary()
        .arg("--web")
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .stdin(std::process::Stdio::piped())
        .stdout(std::fs::File::create(&log).expect("transcript file"))
        .stderr(std::fs::File::create(home.join("stderr.txt")).expect("stderr file"))
        .spawn()
        .expect("failed to run flint");

    let (port, token) = port_and_token(&wait_for_url(&log));
    let mut watching = http_stream(port, "/events", &token);
    let _opening = read_until(&mut watching, "\"type\":\"state\"}\n\n", 20);

    // Opening the page says nothing, so it writes nothing. A home that collected an empty session
    // every time someone looked at the page was a listing of conversations that never happened.
    assert!(
        jsonl_files(&sessions).is_empty(),
        "opening the page wrote a session before anything was said"
    );

    // One question, answered by the stub: this is what makes the file worth keeping, and the switch
    // below is what has to keep it *there* rather than in a second one.
    let asked = post_message(port, &token, "hello");
    assert!(asked.starts_with("HTTP/1.1 202"), "the question was refused: {asked:?}");
    read_until(&mut watching, "STUB ANSWER", 20);

    let before = jsonl_files(&sessions);
    assert_eq!(
        before.len(),
        1,
        "one question wrote {} conversations: {before:?}",
        before.len()
    );
    let was = std::fs::read_to_string(&before[0]).expect("the session file");
    assert!(
        was.contains("\"content\":\"hello\""),
        "the question was not written into the conversation it was asked in: {was:?}"
    );

    // The provider switch, exactly as the header's picker sends it.
    let switched = post_message(port, &token, "/provider other");
    assert!(switched.starts_with("HTTP/1.1 202"), "the switch was refused: {switched:?}");
    let told = read_until(&mut watching, "\"provider\":\"other\"", 20);

    // Read before the scratch home is removed below: the point of the two assertions on this text is
    // that it is the *same* file, appended to rather than rewritten.
    let after = jsonl_files(&sessions);
    let text = after
        .first()
        .map(|path| std::fs::read_to_string(path).unwrap_or_default())
        .unwrap_or_default();
    drop(watching);
    drop(child.stdin.take());
    let exited = wait_for_exit(&mut child, 20);
    let transcript = std::fs::read_to_string(&log).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&home);

    assert!(exited, "flint did not exit");
    assert!(
        told.contains("\"provider\":\"other\"") && told.contains("\"model\":\"other-model\""),
        "the page was not told the run moved to the other provider: {told:?} Terminal: {transcript:?}"
    );
    assert_eq!(
        after.len(),
        1,
        "switching provider started a second conversation: {} files with the same messages in \
         them. Terminal: {transcript:?}",
        after.len()
    );
    assert_eq!(after[0], before[0], "the conversation moved to another file");
    assert!(
        text.starts_with(&was),
        "the file was rewritten rather than appended to, so it is no longer hand-editable history"
    );
    assert!(
        text.contains("\"type\":\"switch\",\"provider\":\"other\",\"model\":\"other-model\""),
        "the file does not record which provider it moved to, so `--resume` would believe the \
         model it started with: {text}"
    );
}

/// What a command answered reaches the page, and a command that fails answers instead of ending
/// the run.
///
/// §8's other half, and the two bugs it fixes are the reason it comes before any button. A
/// command's output went to `printer.term()` and existed nowhere else, so a command typed into the
/// page's composer printed nothing the page could read; and a command that *failed* returned its
/// error out of the REPL loop, where it became the process's exit status -- one mistyped word
/// (`/verbose loud`) took the whole session with it, and the page could not even say why, because
/// the message went to stderr. A button that could do that to a run would be worse than no button,
/// which is why the channel is built before the controls that need it.
///
/// A process, and not a function, because that is where both bugs were: the frames have to arrive
/// on the live stream the page watches, from the same REPL the composer writes into.
#[tokio::test]
async fn the_page_is_told_what_a_command_answered() {
    let home = test_home("command-answer", "http://127.0.0.1:9/v1");
    let log = home.join("transcript.txt");
    let mut child = binary()
        .arg("--web")
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .stdin(std::process::Stdio::piped())
        .stdout(std::fs::File::create(&log).expect("transcript file"))
        .stderr(std::fs::File::create(home.join("stderr.txt")).expect("stderr file"))
        .spawn()
        .expect("failed to run flint");

    let (port, token) = port_and_token(&wait_for_url(&log));
    let mut watching = http_stream(port, "/events", &token);
    // Wait for the opening snapshot, so what is read below is the answer to a command and not the
    // page's own backlog.
    read_until(&mut watching, "\"type\":\"state\"", 20);

    let reported = post_message(port, &token, "/config");
    // To the answer itself, not to the type: a frame carrying no text would pass a test that only
    // looked for `"type":"command"`, and the text is the whole point of the channel.
    let answered = read_until(&mut watching, "\"input\":\"/config\"", 20);

    // Then a command that fails. It is an answer too -- and the run has to still be there
    // afterwards, which is what the state frame below is read for: only a live run sends one.
    let mistyped = post_message(port, &token, "/verbose loud");
    let refused = read_until(&mut watching, "\"input\":\"/verbose loud\"", 20);
    post_message(port, &token, "/verbose full");
    let after = read_until(&mut watching, "\"value\":\"full\"", 20);

    drop(watching);
    drop(child.stdin.take());
    let exited = wait_for_exit(&mut child, 20);
    let transcript = std::fs::read_to_string(&log).unwrap_or_default();
    let complaints = std::fs::read_to_string(home.join("stderr.txt")).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&home);

    assert!(exited, "flint did not exit. Terminal: {transcript:?}");
    assert!(
        reported.starts_with("HTTP/1.1 202"),
        "the command a page sends was not accepted: {reported:?}"
    );
    assert!(
        answered.contains("\"type\":\"command\""),
        "the page was never told what the command answered, which is the whole of §8's other half: \
         {answered:?} Terminal: {transcript:?}"
    );
    assert!(
        answered.contains("config.toml"),
        "the frame carries no answer, only the fact that there was one: {answered:?}"
    );
    assert!(
        refused.contains("\"type\":\"command\"") && refused.contains("expected on|off|full"),
        "a command that failed was not answered on the page: {refused:?}"
    );
    assert!(
        after.contains("\"key\":\"verbose\"") && after.contains("\"value\":\"full\""),
        "the run did not survive a mistyped command, so the session ended the way it used to: \
         {after:?} stderr: {complaints:?}"
    );
    assert!(
        mistyped.starts_with("HTTP/1.1 202"),
        "the mistake was refused at the route rather than answered by the command: {mistyped:?}"
    );
}

/// The state frame out of a live stream, whole.
///
/// `commands_in` below is this for one field; a test that has to ask about two fields of the same
/// frame -- which row is on which screen, and what each setting is -- reads the object once.
fn state_in(frame: &str) -> serde_json::Value {
    for line in frame.lines() {
        let Some(json) = line.strip_prefix("data: ") else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
            continue;
        };
        if value.get("type").and_then(|t| t.as_str()) == Some("state") {
            return value;
        }
    }
    serde_json::Value::Null
}

/// The command list out of a state frame: `(label, send, help, class)` per row.
///
/// Parsed rather than pattern-matched, because the second test below feeds what this returns
/// straight back to the process: what the page may send has to be exactly what the frame said, or
/// the test would be checking a string this file made up.
fn commands_in(frame: &str) -> Vec<(String, String, String, String)> {
    let field = |row: &serde_json::Value, name: &str| {
        row.get(name)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    for line in frame.lines() {
        let Some(json) = line.strip_prefix("data: ") else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
            continue;
        };
        if value.get("type").and_then(|t| t.as_str()) != Some("state") {
            continue;
        }
        let Some(rows) = value.get("commands").and_then(|c| c.as_array()) else {
            continue;
        };
        return rows
            .iter()
            .map(|row| {
                (
                    field(row, "label"),
                    field(row, "send"),
                    field(row, "help"),
                    field(row, "class"),
                )
            })
            .collect();
    }
    Vec::new()
}

/// The page is told that `/say` takes an address, and where the addresses come from.
///
/// `ROADMAP.md` §11 item 9(ii): the page's `/say` had no `--to` because a pid typed into a text field
/// would have been *prose*, and the page had no way to tell an address from a sentence. The frame now
/// says which answer follows a **flag** and which **list** its choices come from, and `GET /peers`
/// answers that list from the same `live::peers_here` the terminal's own `/say` addresses and its
/// reply describes -- so a page cannot offer a pid the terminal would not. The route is asserted to be
/// a listing rather than an exact body on purpose: this test runs in the repository directory, where
/// other tests' runs share this directory's mailbox, and "nobody else is here" would be a coin flip.
#[tokio::test]
async fn the_page_is_told_that_say_takes_an_address_and_where_addresses_come_from() {
    let home = test_home("say-picker", "http://127.0.0.1:9/v1");
    let log = home.join("transcript.txt");
    let mut child = binary()
        .arg("--web")
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .stdin(std::process::Stdio::piped())
        .stdout(std::fs::File::create(&log).expect("transcript file"))
        .stderr(std::fs::File::create(home.join("stderr.txt")).expect("stderr file"))
        .spawn()
        .expect("failed to run flint");

    let (port, token) = port_and_token(&wait_for_url(&log));
    let mut watching = http_stream(port, "/events", &token);
    let opening = read_until(&mut watching, "\"type\":\"state\"}\n\n", 20);
    // The route the picker reads, asked while the run is up. It is the page's own request -- the same
    // headers, the same route -- so a page that could not read it would fail here rather than in a
    // browser nobody runs in CI.
    let mut peers = http_stream(port, "/peers", &token);
    let listing = read_until(&mut peers, "\"peers\":[", 20);
    post_message(port, &token, "/help");
    read_until(&mut watching, "\"input\":\"/help\"", 20);

    drop(peers);
    drop(watching);
    drop(child.stdin.take());
    let exited = wait_for_exit(&mut child, 20);
    let transcript = std::fs::read_to_string(&log).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&home);

    assert!(exited, "flint did not exit");
    assert!(
        listing.contains("200") && listing.contains("\"peers\":["),
        "the page cannot read who is here, so `/say --to` has nothing to offer: {listing:?}"
    );
    // Each entry is a run, and the pid is what an address is made of: without it the picker would
    // offer names that cannot be sent.
    for entry in listing.split("\"pid\"").skip(1) {
        assert!(
            entry.contains(':'),
            "a peer entry carries no pid, which is the one field the command takes: {entry:?}"
        );
    }
    // The frame says which answer follows a flag, and that its choices come from a list. serde writes
    // keys in alphabetical order, and both are read here rather than a shape this test made up.
    assert!(
        opening.contains(
            "\"fields\":[{\"field\":\"text\",\"flag\":\"--to\",\"from\":\"peers\",\"name\":\"pid\",\
             \"optional\":true},{\"field\":\"text\",\"name\":\"text\",\"optional\":false}]"
        ),
        "the page is not told that `/say` takes an address or where to get one: {opening:?}"
    );
    assert!(
        opening.contains("\"label\":\"/say [--to <pid>] <text>\",\"send\":\"/say\""),
        "the row the page draws does not name the address it takes: {opening:?}"
    );
    // And the terminal's own help says the same thing, because the two are one table.
    assert!(
        transcript.contains("/say [--to <pid>] <text>"),
        "`/help` does not offer the address the page now does: {transcript:?}"
    );
}

/// The page is handed the command list, and it is the same list `/help` prints.
///
/// §8's read channel, second half: a picker needs the values a setting takes, and a menu needs the
/// commands there are. The frame carries each one as what `/help` prints (`label`), what the page
/// would put on the wire (`send`), what it does (`help`) and which of §8's classes it belongs to
/// (`class`) — `send` separate from `label` because the split is not uniform: `add` is part of
/// `/provider add` and `<key>` is not, and a page that had to tell those apart would be
/// re-deriving the terminal's own grammar.
///
/// The cross-check at the end is the point of the test: the terminal's `/help` and the page's menu
/// are one table, and a second copy of it is how a page comes to offer a command that has been
/// renamed. Both halves are read here — the frame off the live stream, `/help` off the run's own
/// stdout — so this fails if either rendering drifts.
#[tokio::test]
async fn the_page_is_told_which_commands_it_may_offer() {
    let home = test_home("command-list", "http://127.0.0.1:9/v1");
    let log = home.join("transcript.txt");
    let mut child = binary()
        .arg("--web")
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .stdin(std::process::Stdio::piped())
        .stdout(std::fs::File::create(&log).expect("transcript file"))
        .stderr(std::fs::File::create(home.join("stderr.txt")).expect("stderr file"))
        .spawn()
        .expect("failed to run flint");

    let (port, token) = port_and_token(&wait_for_url(&log));
    let mut watching = http_stream(port, "/events", &token);
    // Read to the *end* of the frame, and not to a key inside it: serde writes the keys in order,
    // so stopping at any field would leave a truncated object that will not parse.
    let opening = read_until(&mut watching, "\"type\":\"state\"}\n\n", 20);
    post_message(port, &token, "/help");
    read_until(&mut watching, "\"input\":\"/help\"", 20);

    drop(watching);
    drop(child.stdin.take());
    let exited = wait_for_exit(&mut child, 20);
    let transcript = std::fs::read_to_string(&log).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&home);

    assert!(exited, "flint did not exit");
    let commands = commands_in(&opening);
    assert!(
        !commands.is_empty(),
        "the state frame carries no command list, so the page has nothing to offer: {opening:?}"
    );
    assert!(
        commands.iter().any(|(label, send, _, class)| label == "/provider key <key>"
            && send == "/provider key"
            && class == "form"),
        "a command that is a form is not carried as one, so a page could only guess at the \
         argument it needs: {commands:?}"
    );
    // `/config set` is the row that closes the last gap in this list. It was a line to type -- "typed
    // in the terminal" -- because the terminal had no way to change one setting from one line, and the
    // wizard behind `/config edit` cannot be answered from a browser. It has one now, so the page is
    // handed the two answers rather than pointed at the terminal, and the value is the *optional* one:
    // a blank value is how a proxy is cleared, and a required field can never be sent empty.
    assert!(
        commands.iter().any(|(label, send, _, class)| label == "/config set <key> <value>"
            && send == "/config set"
            && class == "form"),
        "the one-line setting command is not carried as a form: {commands:?}"
    );
    assert!(
        opening.contains(
            "\"class\":\"form\",\"fields\":[{\"field\":\"text\",\"name\":\"key\",\
             \"optional\":false},{\"field\":\"text\",\"name\":\"value\",\"optional\":true}],\
             \"help\":\"change one setting, and use it now\",\
             \"label\":\"/config set <key> <value>\",\"send\":\"/config set\""
        ),
        "the page is not told what `/config set` takes, so it can only offer it as a line to type: \
         {opening:?}"
    );
    assert!(
        commands.iter().any(|(_, _, _, class)| class == "danger"),
        "no command is marked destructive, so a control would have nothing to confirm: \
         {commands:?}"
    );
    assert!(
        commands.iter().any(|(label, send, _, class)| label == "/reload"
            && send == "/reload"
            && class == "button"),
        "a no-argument action is not carried, which is the class §8 builds first: {commands:?}"
    );
    // The classes the page cannot draw from *this* frame are not in it. `/exit` is the one §8 calls
    // out (a window onto a process, and a misclick must not end a session); the switches are already
    // on the page from the `settings` field, and repeating them here would be one fact in two places;
    // `/web` has nothing to offer (the page *is* the web view) and `!` is a shell escape that the
    // composer can type anyway.
    for absent in [
        "\"label\":\"/exit\"",
        "\"label\":\"/verbose",
        "\"label\":\"/detail",
        "\"label\":\"/readonly",
        "\"label\":\"/hear-peers",
        "\"label\":\"/thinking",
        "\"label\":\"/web\"",
        "\"label\":\"!<command>\"",
    ] {
        assert!(
            !opening.contains(absent),
            "the frame offers {absent}, which the page either must not offer or already has: \
             {opening:?}"
        );
    }
    // Which screen a row is on is the frame's answer too, and the rows that are on none are the
    // palette's own: a row the dialog files nowhere is still offered by the `/` menu, and the group
    // is what tells the two apart. Checked per row rather than counted, because a group on the wrong
    // row is a command on the wrong screen.
    let frame = state_in(&opening);
    let group_of = |label: &str| -> Option<String> {
        frame
            .get("commands")
            .and_then(|c| c.as_array())
            .and_then(|rows| {
                rows.iter()
                    .find(|row| row.get("label").and_then(|l| l.as_str()) == Some(label))
            })
            .and_then(|row| row.get("group").and_then(|g| g.as_str()))
            .map(str::to_string)
    };
    assert_eq!(
        group_of("/provider key <key>").as_deref(),
        Some("model"),
        "the key row is not on the screen that holds the endpoint: {opening:?}"
    );
    assert_eq!(
        group_of("/jobs stop <pid>").as_deref(),
        Some("work"),
        "the stop is not filed with the jobs it stops: {opening:?}"
    );
    assert_eq!(
        group_of("/help"),
        None,
        "the frame files `/help` on a settings screen, and the dialog is not a palette: {opening:?}"
    );
    // The other half of the frame: what a page may *change* rather than what it may send. Every
    // setting carries the words that change it, the values it may take, the value in force, the
    // sentence `/help` prints, and the screen it belongs on -- and there is one per setting, with the
    // four keys `/config set` takes among them, or a person looking for `max_steps` finds it in the
    // terminal and nowhere else.
    let settings = frame
        .get("settings")
        .and_then(|s| s.as_array())
        .cloned()
        .unwrap_or_default();
    for (key, group, kind) in [
        ("provider", "model", "select"),
        ("model", "model", "select"),
        ("thinking", "model", "select"),
        ("verbose", "run", "select"),
        ("detail", "run", "select"),
        ("readonly", "run", "select"),
        ("hear-peers", "run", "select"),
        ("shell", "limits", "text"),
        ("shell_args", "limits", "text"),
        ("max_steps", "limits", "number"),
        ("proxy", "limits", "text"),
    ] {
        let found = settings
            .iter()
            .find(|setting| setting.get("key").and_then(|k| k.as_str()) == Some(key))
            .unwrap_or_else(|| panic!("the frame has no `{key}` setting: {settings:?}"));
        let text = |name: &str| found.get(name).and_then(|v| v.as_str()).unwrap_or_default();
        assert_eq!(text("group"), group, "`{key}` is on the wrong screen: {found:?}");
        assert_eq!(text("kind"), kind, "`{key}` is drawn as the wrong input: {found:?}");
        assert!(
            !text("send").is_empty(),
            "`{key}` has no words that change it: {found:?}"
        );
        assert!(
            !text("help").is_empty(),
            "`{key}` has no sentence to show: {found:?}"
        );
        // A choice must carry the choices -- a `<select>` with none is a control that lies about the
        // run -- and a value that is not a choice must carry none, or the page draws a picker whose
        // only option is the value already in force.
        assert_eq!(
            found.get("choices").is_some(),
            kind == "select",
            "`{key}` carries choices on the wrong kind of setting: {found:?}"
        );
    }
    // And the terminal's own help is the same table: every row the page was handed is a row
    // `/help` prints, with the same one-line description. The one row that may differ is the one
    // whose sentence names the provider in force: `/provider key`'s help says "the active provider",
    // which means nothing to somebody looking at a browser, so the frame substitutes the name. Every
    // other row must match word for word -- this loop is what keeps the page from growing a second
    // description of a command that drifts from `/help`.
    //
    // `/help` wraps a description that is longer than its column, so a long sentence arrives in two
    // pieces; joining them back up with the column it wraps into is what makes the comparison about
    // the *words*. The alternative -- keeping every description short enough to fit -- would mean
    // choosing the help text to suit the test.
    let flat = transcript.replace("\n                        ", " ");
    for (label, _, help, _) in &commands {
        assert!(
            flat.contains(label.as_str()),
            "the page is offered `{label}` and `/help` does not print it, so the two lists have \
             drifted apart: {transcript:?}"
        );
        // `test_home` names the provider `stub`, which is the name that would have been substituted.
        let unsubstituted = help.replace("stub", "the active provider");
        assert!(
            flat.contains(help.as_str()) || flat.contains(unsubstituted.as_str()),
            "`{label}` is described one way to the page and another in `/help`: {transcript:?}"
        );
    }
    // And the words a *setting* is changed with are words `/help` prints, on the same authority: the
    // frame hands the page the line rather than letting it spell `<key> <value>` out for itself, and a
    // line that no longer exists would be a control that answers with a refusal. The head of a
    // `/config set <key>` line is the command; the key after it is the setting's own name, which
    // `/help` prints once, in the command's usage line.
    for setting in &settings {
        let key = setting.get("key").and_then(|k| k.as_str()).unwrap_or_default();
        let send = setting.get("send").and_then(|s| s.as_str()).unwrap_or_default();
        let head = send.split(' ').take(2).collect::<Vec<_>>().join(" ");
        assert!(
            flat.contains(&format!("{send} ")) || flat.contains(&format!("{head} ")),
            "`{key}` is changed by `{send}`, which `/help` does not print: {transcript:?}"
        );
    }
}

/// A report the page asks for is answered to the page, and *not* printed here.
///
/// §8's panel class. The page shows a listing in a panel of its own rather than sending it into the
/// terminal, because the terminal is where somebody typed `/help` and the page's reader did not ask
/// for it there. So there are two routes to the same command and the difference is exactly that:
/// `POST /message` is a person typing, and what it prints lands in the transcript here;
/// `POST /report` is the page reading, and its answer goes to the feed only.
///
/// The assertion that matters is the count. `/config`'s answer is posted both ways in one run, and
/// the transcript must carry it exactly *once* -- if the report route printed as well, a reader
/// would see the listing twice and the page would be a second thing to keep in step rather than a
/// window onto the run.
///
/// The refusal is checked in the same run because it is the safety property: the page has no
/// confirmation step yet, so a report route that ran whatever it was given would be a way to delete
/// a conversation with one click that never happened. Only the rows the table marks as reports are
/// run; anything else is answered back, and the proof is that a button command asked for as a
/// report does not do its work.
#[tokio::test]
async fn a_report_the_page_asks_for_is_not_printed_here() {
    let home = test_home("report-route", "http://127.0.0.1:9/v1");
    let log = home.join("transcript.txt");
    let mut child = binary()
        .arg("--web")
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .stdin(std::process::Stdio::piped())
        .stdout(std::fs::File::create(&log).expect("transcript file"))
        .stderr(std::fs::File::create(home.join("stderr.txt")).expect("stderr file"))
        .spawn()
        .expect("failed to run flint");

    let (port, token) = port_and_token(&wait_for_url(&log));
    let mut watching = http_stream(port, "/events", &token);
    read_until(&mut watching, "\"type\":\"state\"}\n\n", 20);

    // Read, not typed: the answer is marked as a panel's, so the page knows to put it in the panel
    // rather than in the transcript.
    let asked = post_to(port, &token, "/report", "/config");
    assert!(
        asked.starts_with("HTTP/1.1 202"),
        "the report route refused a report: {asked:?}"
    );
    let read = read_until(&mut watching, "\"input\":\"/config\"", 20);
    assert!(
        read.contains("\"panel\":true") && read.contains("config.toml"),
        "the page asked for a report and was not told it, or was not told where to put it: {read:?}"
    );

    // The same command, typed: this one belongs in the transcript, and it is the one that proves the
    // report above was quiet rather than merely late.
    let typed = post_message(port, &token, "/config");
    assert!(typed.starts_with("HTTP/1.1 202"), "the composer refused: {typed:?}");
    let echoed = read_until(&mut watching, "\"input\":\"/config\"", 20);
    assert!(
        !echoed.contains("\"panel\":true") && echoed.contains("config.toml"),
        "a command typed into the composer was answered as a panel report: {echoed:?}"
    );

    // And the safety half: a command the table does *not* mark as a report is answered rather than
    // run. `/new` is a button, and a button's work is visible -- it starts a session and says so.
    let refused = post_to(port, &token, "/report", "/new");
    assert!(
        refused.starts_with("HTTP/1.1 202"),
        "the report route refused the request itself rather than the command: {refused:?}"
    );
    let complaint = read_until(&mut watching, "\"input\":\"/new\"", 20);
    assert!(
        complaint.contains("not a report"),
        "a command that is not a report was not refused, so the route would run anything: \
         {complaint:?}"
    );

    drop(watching);
    drop(child.stdin.take());
    let exited = wait_for_exit(&mut child, 20);
    let transcript = std::fs::read_to_string(&log).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&home);

    assert!(exited, "flint did not exit");
    assert_eq!(
        transcript.matches("config.toml").count(),
        1,
        "the report was printed on this terminal as well as sent to the page, so a listing nobody \
         asked for here appeared here anyway: {transcript:?}"
    );
    assert!(
        !transcript.contains("started a new session"),
        "the refused report ran anyway: {transcript:?}"
    );
}

/// A selector the page can choose from: `/skills <name>`.
///
/// §8's selector class for the one value-taking command whose options the frame now describes. The
/// names come from the run's own walk of the skill directories -- the same walk as its system prompt,
/// which is why the menu cannot offer a skill the model was never told about -- and the values are
/// the *permission* as well as the options: the page may read the command with one of them, and with
/// nothing else.
///
/// The second half is asserted with a skill that appears **after** the run started. That is the case
/// which tells the two rules apart: the command itself discovers the directory fresh, so a loose
/// check would happily read a name the menu never offered, while the rule here is that the menu *is*
/// the permission. `/reload` is what makes a new skill appear, and that is the documented way to pick
/// one up.
#[tokio::test]
async fn a_skill_the_run_has_is_readable_from_the_page_and_nothing_else_is() {
    let home = test_home("report-skills", "http://127.0.0.1:9/v1");
    let skill = home.join("skills").join("demo");
    std::fs::create_dir_all(&skill).expect("skill directory");
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: demo\ndescription: a fixture skill\n---\n\nDo the demo thing.\n",
    )
    .expect("skill file");

    let log = home.join("transcript.txt");
    let mut child = binary()
        .arg("--web")
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .stdin(std::process::Stdio::piped())
        .stdout(std::fs::File::create(&log).expect("transcript file"))
        .stderr(std::fs::File::create(home.join("stderr.txt")).expect("stderr file"))
        .spawn()
        .expect("failed to run flint");

    let (port, token) = port_and_token(&wait_for_url(&log));
    let mut watching = http_stream(port, "/events", &token);
    let opening = read_until(&mut watching, "\"type\":\"state\"}\n\n", 20);

    // The value rides on the row that takes one. The frame's keys are alphabetical, so this fragment
    // is the whole row: the label the page shows, the line it sends, and the one name it may append.
    assert!(
        opening.contains("\"label\":\"/skills [name]\",\"send\":\"/skills\",\"values\":[\"demo\"]"),
        "the run has a skill and the page's menu does not offer it, so a selector would have \
         nothing to choose from: {opening:?}"
    );

    // Reading it: the body arrives as a panel's, and the terminal never sees it.
    let asked = post_to(port, &token, "/report", "/skills demo");
    assert!(asked.starts_with("HTTP/1.1 202"), "the report route refused it: {asked:?}");
    let read = read_until(&mut watching, "\"input\":\"/skills demo\"", 20);
    assert!(
        read.contains("\"panel\":true") && read.contains("Do the demo thing."),
        "asking for a skill the menu offered did not come back as a reading: {read:?}"
    );

    // A skill that exists on disk and was never in the menu. The command would read it -- `/skills`
    // discovers the directories on every call -- so this is the assertion that the *menu* is the
    // permission rather than whatever the filesystem happens to hold at press time.
    let late = home.join("skills").join("late");
    std::fs::create_dir_all(&late).expect("late skill directory");
    std::fs::write(late.join("SKILL.md"), "---\nname: late\n---\n\nDo the late thing.\n")
        .expect("late skill file");
    let refused = post_to(port, &token, "/report", "/skills late");
    assert!(refused.starts_with("HTTP/1.1 202"), "the request itself was refused: {refused:?}");
    let complaint = read_until(&mut watching, "\"input\":\"/skills late\"", 20);
    assert!(
        complaint.contains("not a report"),
        "a skill the menu never offered was read, so the page may compose any argument it likes \
         and the frame's values are decoration: {complaint:?}"
    );

    drop(watching);
    drop(child.stdin.take());
    let exited = wait_for_exit(&mut child, 20);
    let transcript = std::fs::read_to_string(&log).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&home);

    assert!(exited, "flint did not exit");
    for never in ["Do the demo thing.", "Do the late thing."] {
        assert_eq!(
            transcript.matches(never).count(),
            0,
            "a listing the page asked for was printed on this terminal as well: {transcript:?}"
        );
    }
}

/// The page can read a saved prompt, and can send one -- and the two are different rows.
///
/// A template is the one command a page cannot type: its invocation is `/<name>`, and a row the page
/// can press sends a fixed `send` plus one value. So the discovery result is offered twice, and the
/// pair is the whole point: `/prompts <name>` is a `panel` row whose value is a **reading** -- the
/// body, in the panel, with the terminal quiet -- while `/prompt <name>` is a `selector` whose value
/// is the command itself, sent as a line and answered in the transcript where the work is. A page
/// that could *read* through the second one would be running a turn through the report route, which
/// is the same boundary `a_switch_is_offered_the_values_it_may_take` holds for `/provider`.
#[tokio::test]
async fn a_saved_prompt_is_readable_and_sendable_from_the_page() {
    let home = test_home("report-prompts", "http://127.0.0.1:9/v1");
    let prompts = home.join("prompts");
    std::fs::create_dir_all(&prompts).expect("prompt directory");
    std::fs::write(
        prompts.join("demo.md"),
        "---\ndescription: a fixture prompt\n---\n\nDo the demo thing with {args}.\n",
    )
    .expect("prompt file");

    let log = home.join("transcript.txt");
    let mut child = binary()
        .arg("--web")
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .stdin(std::process::Stdio::piped())
        .stdout(std::fs::File::create(&log).expect("transcript file"))
        .stderr(std::fs::File::create(home.join("stderr.txt")).expect("stderr file"))
        .spawn()
        .expect("failed to run flint");

    let (port, token) = port_and_token(&wait_for_url(&log));
    let mut watching = http_stream(port, "/events", &token);
    let opening = read_until(&mut watching, "\"type\":\"state\"}\n\n", 20);

    // Both rows, each with the one name this run found. The frame's keys are alphabetical, so each
    // fragment is the whole row.
    assert!(
        opening.contains("\"label\":\"/prompts [name]\",\"send\":\"/prompts\",\"values\":[\"demo\"]"),
        "the page's menu does not offer the saved prompt at all: {opening:?}"
    );
    assert!(
        opening.contains("\"label\":\"/prompt <name> [args]\",\"send\":\"/prompt\",\"values\":[\"demo\"]"),
        "there is no row that sends a saved prompt, so a page could read one and never use it: \
         {opening:?}"
    );

    // Reading it: the body arrives as a panel's.
    let asked = post_to(port, &token, "/report", "/prompts demo");
    assert!(asked.starts_with("HTTP/1.1 202"), "the report route refused it: {asked:?}");
    let read = read_until(&mut watching, "\"input\":\"/prompts demo\"", 20);
    assert!(
        read.contains("\"panel\":true") && read.contains("Do the demo thing with {args}."),
        "asking for a prompt the menu offered did not come back as a reading: {read:?}"
    );

    // Sending it is not a reading: the same value on the other row must not run a turn in the quiet.
    let sent = post_to(port, &token, "/report", "/prompt demo");
    assert!(sent.starts_with("HTTP/1.1 202"), "the request itself was refused: {sent:?}");
    let refused = read_until(&mut watching, "\"input\":\"/prompt demo\"", 20);
    assert!(
        refused.contains("not a report"),
        "a saved prompt was sent through the report route, which is a turn nobody asked for: \
         {refused:?}"
    );

    drop(watching);
    drop(child.stdin.take());
    let exited = wait_for_exit(&mut child, 20);
    let transcript = std::fs::read_to_string(&log).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&home);

    assert!(exited, "flint did not exit");
    assert_eq!(
        transcript.matches("Do the demo thing").count(),
        0,
        "a listing the page asked for was printed on this terminal as well: {transcript:?}"
    );
}

/// A switch is offered the values it may take, and a switch is not a reading.
///
/// `/provider <name>` and `/model <name>` take an argument out of a list this run already knows, and
/// the page is shown that list twice over: the header's pickers are filled from `providers`, and the
/// rows now carry the same names as `values`. Asking somebody to read a name off one control and type
/// it into another is the kind of thing this round exists to remove.
///
/// The second half is the interesting one. A `values` row used to mean "a read the page may ask for",
/// and the report route runs a command **with the terminal quiet** — which for `/provider llamacpp`
/// means starting a local engine without a word anywhere. So the values alone are not the permission:
/// a value is a reading only on a `panel` row. These rows are `selector`s, the page types the line it
/// was given, and the answer lands in the transcript where the change is. The refusal below is that
/// boundary, asserted at the route rather than reasoned about.
#[tokio::test]
async fn a_switch_is_offered_the_values_it_may_take() {
    let home = test_home("switch-values", "http://127.0.0.1:9/v1");
    std::fs::write(
        home.join("config.toml"),
        "default_provider = \"stub\"\n\n\
         [[providers]]\n\
         name = \"stub\"\n\
         base_url = \"http://127.0.0.1:9/v1\"\n\
         model = \"stub-model\"\n\
         models = [\"stub-other\"]\n\
         api_key = \"not-a-real-key\"\n\n\
         [[providers]]\n\
         name = \"other\"\n\
         base_url = \"http://127.0.0.1:9/v1\"\n\
         model = \"other-model\"\n\
         api_key = \"not-a-real-key\"\n",
    )
    .expect("a config with two providers");
    let log = home.join("transcript.txt");
    let mut child = binary()
        .arg("--web")
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .stdin(std::process::Stdio::piped())
        .stdout(std::fs::File::create(&log).expect("transcript file"))
        .stderr(std::fs::File::create(home.join("stderr.txt")).expect("stderr file"))
        .spawn()
        .expect("failed to run flint");

    let (port, token) = port_and_token(&wait_for_url(&log));
    let mut watching = http_stream(port, "/events", &token);
    let opening = read_until(&mut watching, "\"type\":\"state\"}\n\n", 20);

    // Every name the run can switch to, in the file's order, on the row that switches.
    assert!(
        opening.contains(
            "\"label\":\"/provider <name>\",\"send\":\"/provider\",\"values\":[\"stub\",\"other\"]"
        ),
        "the page cannot switch provider without the reader typing a name it was never shown: \
         {opening:?}"
    );
    // The models of the provider in force, from the same `choices()` the picker and `/model` use --
    // the active one first, and the extras the config lists after it.
    assert!(
        opening.contains(
            "\"label\":\"/model <name>\",\"send\":\"/model\",\"values\":[\"stub-model\",\"stub-other\"]"
        ),
        "the page cannot switch model without the reader typing one: {opening:?}"
    );
    // And the listing is still a listing: `/model` on its own is a report, and carries no values.
    assert!(
        opening.contains(
            "\"class\":\"panel\",\"group\":\"model\",\"help\":\"show the model in force\",\
             \"label\":\"/model\",\"send\":\"/model\""
        ),
        "`/model` is no longer offered as a report: {opening:?}"
    );

    // And the key row says *which* provider's key it is for. `/help` says "the active provider",
    // which is a phrase somebody looking at a browser cannot resolve -- and which provider the key
    // belongs to is the one thing that reader needs before pasting a credential into a box.
    assert!(
        opening.contains(
            "\"class\":\"form\",\"fields\":[{\"field\":\"password\",\"name\":\"key\",\
             \"optional\":false}],\"group\":\"model\",\"help\":\"set the API key for stub\",\
             \"label\":\"/provider key <key>\",\"send\":\"/provider key\""
        ),
        "the key row does not name the provider it is for, so a masked box appears with no way to \
         tell whose key goes in it: {opening:?}"
    );

    // A value on a selector row is typed, not read: the report route refuses it, which is what keeps
    // a local engine from being started with the terminal quiet.
    let refused = post_to(port, &token, "/report", "/provider other");
    assert!(refused.starts_with("HTTP/1.1 202"), "the request itself was refused: {refused:?}");
    let complaint = read_until(&mut watching, "\"input\":\"/provider other\"", 20);
    assert!(
        complaint.contains("not a report"),
        "a provider switch was run as a reading, so the page may start an engine quietly: \
         {complaint:?}"
    );

    drop(watching);
    drop(child.stdin.take());
    let exited = wait_for_exit(&mut child, 20);
    let _ = std::fs::remove_dir_all(&home);

    assert!(exited, "flint did not exit");
}

/// Adding a provider is something a page can do, and what it writes is a provider.
///
/// `/provider add` used to be the interactive wizard and nothing else, so the page could only point at
/// the terminal and say "it asks questions". One line's worth of the same five answers is the whole
/// difference: `add <name> <base_url> [model]` writes the provider the wizard would have written, and
/// the frame carries the three answers as `fields` so the page can ask for each of them. The key is
/// not one of them -- a credential does not go in a transcript, and `/provider key` is the masked row
/// that already exists -- which is why adding *switches* to the new provider: `/provider key` sets the
/// key of the provider in force, so without the switch the very next press would set somebody else's.
///
/// The line posted below is the one the page composes from its three inputs, in the frame's order,
/// with the optional answer left off; the second post is the same command typed by hand, to show the
/// two forms are one command. The last assertion is the one that ties this to the fix before it: a
/// provider added mid-conversation is a `switch` in the file the conversation is already in -- and the
/// conversation is asked a question first, because a run that has said nothing has no file for the
/// switch to be appended to.
#[tokio::test]
async fn a_provider_can_be_added_from_the_page() {
    let server = MockServer::start().await;
    answer_once(&server).await;
    let home = test_home("provider-add", &server.uri());
    let log = home.join("transcript.txt");
    let mut child = binary()
        .arg("--web")
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .stdin(std::process::Stdio::piped())
        .stdout(std::fs::File::create(&log).expect("transcript file"))
        .stderr(std::fs::File::create(home.join("stderr.txt")).expect("stderr file"))
        .spawn()
        .expect("failed to run flint");

    let (port, token) = port_and_token(&wait_for_url(&log));
    let mut watching = http_stream(port, "/events", &token);
    let opening = read_until(&mut watching, "\"type\":\"state\"}\n\n", 20);

    // The frame's half: three answers, named, in the order the line wants them, and only the last one
    // optional -- the page has to know which ones it may leave empty, because the empty form is the
    // wizard, and a wizard in a served run is a question nobody can answer.
    assert!(
        opening.contains(
            "\"fields\":[{\"field\":\"text\",\"name\":\"name\",\"optional\":false},\
             {\"field\":\"text\",\"name\":\"base_url\",\"optional\":false},\
             {\"field\":\"text\",\"name\":\"model\",\"optional\":true}]"
        ),
        "the page is not told what `/provider add` takes, so it can only offer it as a line to type: \
         {opening:?}"
    );

    let sessions = home.join("sessions");
    // Nothing has been said yet -- opening the page is not saying anything -- so there is no session
    // to find. The question below writes the file, and the provider added after it goes into that same
    // file rather than starting one of its own.
    assert!(
        jsonl_files(&sessions).is_empty(),
        "opening the page wrote a session before anything was said"
    );
    let asked = post_message(port, &token, "hello");
    assert!(asked.starts_with("HTTP/1.1 202"), "the question was refused: {asked:?}");
    read_until(&mut watching, "STUB ANSWER", 20);
    assert_eq!(
        jsonl_files(&sessions).len(),
        1,
        "one question did not leave exactly one conversation"
    );

    // The line the page composes: the row's own `send`, then the answers, with the empty optional one
    // left off. Nineteen is a base address; nothing is ever sent to it.
    let posted = post_message(port, &token, "/provider add claw http://127.0.0.1:9/v1");
    assert!(posted.starts_with("HTTP/1.1 202"), "the add was refused: {posted:?}");
    let told = read_until(&mut watching, "\"provider\":\"claw\"", 20);

    let after = jsonl_files(&sessions);
    let text = after
        .first()
        .map(|path| std::fs::read_to_string(path).unwrap_or_default())
        .unwrap_or_default();

    // A second add of the same name, typed by hand: one command, two ways to reach it, and the mistake
    // a page makes by pressing twice is refused rather than quietly overwriting the entry.
    //
    // Each posted line is waited for before the next one goes, and that is not tidiness: a line from the
    // page is *queued* for the loop that owns the commands, and closing the input is what ends the run.
    // Closing it while a line is still in the queue drops that line, and the test then reads a config
    // file the command never reached -- which is how this assertion failed on the Linux runner only,
    // where the queue lost the race that a faster machine wins.
    post_message(port, &token, "/provider add claw http://127.0.0.1:9/v1");
    let refused = read_until(
        &mut watching,
        "\"input\":\"/provider add claw http://127.0.0.1:9/v1\"",
        20,
    );
    // And the model, which the first line left off, is the wizard's own default rather than nothing.
    post_message(port, &token, "/provider add second http://127.0.0.1:9/v1 second-model");
    let added = read_until(&mut watching, "\"provider\":\"second\"", 20);

    drop(watching);
    drop(child.stdin.take());
    let exited = wait_for_exit(&mut child, 20);
    let transcript = std::fs::read_to_string(&log).unwrap_or_default();
    let config = std::fs::read_to_string(home.join("config.toml")).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&home);

    assert!(exited, "flint did not exit");
    assert!(
        refused.contains("already"),
        "adding the same provider twice was not refused, so a second press of the page's own button \
         quietly rewrote the entry: {refused:?} Terminal: {transcript:?}"
    );
    assert!(
        added.contains("\"provider\":\"second\""),
        "the second provider was never switched to, so the line the page composed did not reach the \
         loop that owns the commands: {added:?} Terminal: {transcript:?}"
    );
    assert!(
        told.contains("\"provider\":\"claw\""),
        "adding a provider did not switch to it, so the key row would go on naming the old one: \
         {told:?} Terminal: {transcript:?}"
    );
    assert!(
        config.contains("name = \"claw\"") && config.contains("model = \"deepseek-chat\""),
        "the provider the page added is not in the config file with a usable model: {config:?}"
    );
    assert!(
        config.contains("name = \"second\"") && config.contains("model = \"second-model\""),
        "a provider added with its model did not keep it: {config:?}"
    );
    assert_eq!(
        config.matches("name = \"claw\"").count(),
        1,
        "adding the same name twice wrote it twice: {config:?}"
    );
    assert!(
        transcript.contains("already exists"),
        "the second add was accepted in silence, so a page pressing twice cannot tell what happened: \
         {transcript:?}"
    );
    assert_eq!(after.len(), 1, "adding a provider started a second conversation: {after:?}");
    assert!(
        text.contains("\"type\":\"meta\""),
        "the file the first thing said begins with something other than `meta`: {text:?}"
    );
    assert!(
        text.contains("\"type\":\"switch\",\"provider\":\"claw\""),
        "the file does not say the conversation moved to the provider that was just added: {text:?}"
    );
}

/// A form the page fills in: `/provider key <key>`, and the key does not come back.
///
/// §8's form class, and the one promise in it that is a promise about *secrets*: the argument is a
/// credential, so it may not be echoed to the page, printed on the terminal, or written anywhere
/// except the config file it is for. The `command` frame echoes the line that asked for the answer,
/// which is exactly right for every other command and exactly wrong for this one, so the table says
/// which rows take a credential and the echo is the row's own `send` instead.
///
/// The three absences are asserted *separately*, because they are three different failures: a frame
/// carrying the key is a leak to every page connected to this run and to the event ring; a key in
/// the transcript is a leak to whoever reads the log; and the fourth assertion -- that the key *did*
/// reach `config.toml` -- is what keeps the first three from being satisfied by a command that never
/// ran.
#[tokio::test]
async fn a_key_typed_into_a_field_is_not_echoed_anywhere() {
    let home = test_home("form-key", "http://127.0.0.1:9/v1");
    let log = home.join("transcript.txt");
    let mut child = binary()
        .arg("--web")
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .stdin(std::process::Stdio::piped())
        .stdout(std::fs::File::create(&log).expect("transcript file"))
        .stderr(std::fs::File::create(home.join("stderr.txt")).expect("stderr file"))
        .spawn()
        .expect("failed to run flint");

    let (port, token) = port_and_token(&wait_for_url(&log));
    let mut watching = http_stream(port, "/events", &token);
    let opening = read_until(&mut watching, "\"type\":\"state\"}\n\n", 20);

    // The frame says the page may fill this one in, and how to draw it -- so the page is not
    // deciding from the command's name that a key is a secret.
    assert!(
        opening.contains("\"label\":\"/provider key <key>\",\"send\":\"/provider key\"")
            && opening.contains("\"field\":\"password\""),
        "the frame does not tell the page that this row takes a credential it may fill in: \
         {opening:?}"
    );

    // A fixture, not a key: no test in this repository contains a real one.
    const NOT_A_KEY: &str = "sk-not-a-real-key-0000";
    let sent = post_message(port, &token, &format!("/provider key {NOT_A_KEY}"));
    assert!(sent.starts_with("HTTP/1.1 202"), "the composer refused: {sent:?}");
    let answered = read_until(&mut watching, "\"input\":\"/provider key\"", 20);
    assert!(
        answered.contains("key saved"),
        "the key was never saved, so the absences below would hold for a command that did nothing: \
         {answered:?}"
    );
    assert!(
        !answered.contains(NOT_A_KEY),
        "the answer echoed the key back to every page watching this run: {answered:?}"
    );

    // And the refusal path: a key posted to the read route is refused -- reading is not what that
    // command does -- and the refusal names the line it refused, which is the other place the key
    // would be printed and sent on.
    let refused = post_to(port, &token, "/report", &format!("/provider key {NOT_A_KEY}"));
    assert!(refused.starts_with("HTTP/1.1 202"), "the request itself was refused: {refused:?}");
    let complaint = read_until(&mut watching, "\"input\":\"/provider key\"", 20);
    assert!(
        complaint.contains("not a report") && !complaint.contains(NOT_A_KEY),
        "the refusal of a key-carrying line repeated the key: {complaint:?}"
    );

    drop(watching);
    drop(child.stdin.take());
    let exited = wait_for_exit(&mut child, 20);
    let transcript = std::fs::read_to_string(&log).unwrap_or_default();
    let config = std::fs::read_to_string(home.join("config.toml")).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&home);

    assert!(exited, "flint did not exit");
    assert!(
        !transcript.contains(NOT_A_KEY),
        "the key was printed on the terminal, where a session log or a shoulder can read it: \
         {transcript:?}"
    );
    assert!(
        config.contains(NOT_A_KEY),
        "the key did not reach the config file, which is the one place it belongs: {config:?}"
    );
}

/// A destructive row says which list its argument comes from, and nothing else does.
///
/// §8's last class. The confirmation itself is the page's -- two deliberate presses, with the line
/// about to be sent printed on the row being pressed -- because a process-side confirmation would be
/// a second way to run `/delete` that the terminal does not have, and because there is no undo
/// anywhere in flint for a *page* to offer one either. What the process has to supply is the part the
/// page cannot work out: which of the two lists it already holds is the one this row takes its
/// argument from. Without it a page would have to recognise `/delete <n|id>` by name, which is the
/// one thing every other control is built to avoid.
#[tokio::test]
async fn a_destructive_row_says_where_its_argument_comes_from() {
    let home = test_home("danger-from", "http://127.0.0.1:9/v1");
    let log = home.join("transcript.txt");
    let mut child = binary()
        .arg("--web")
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .stdin(std::process::Stdio::piped())
        .stdout(std::fs::File::create(&log).expect("transcript file"))
        .stderr(std::fs::File::create(home.join("stderr.txt")).expect("stderr file"))
        .spawn()
        .expect("failed to run flint");

    let (port, token) = port_and_token(&wait_for_url(&log));
    let mut watching = http_stream(port, "/events", &token);
    let opening = read_until(&mut watching, "\"type\":\"state\"}\n\n", 20);
    drop(watching);
    drop(child.stdin.take());
    let exited = wait_for_exit(&mut child, 20);
    let _ = std::fs::remove_dir_all(&home);

    assert!(exited, "flint did not exit");
    // The frame's keys are alphabetical, so each fragment is one whole row.
    assert!(
        opening.contains(
            "\"from\":\"sessions\",\"group\":\"conversation\",\"help\":\"delete one\",\
             \"label\":\"/delete <n|id>\",\"send\":\"/delete\""
        ) && opening.contains(
            "\"from\":\"sessions\",\"group\":\"conversation\",\
             \"help\":\"file one away, out of the list\",\"label\":\"/archive <n|id>\",\
             \"send\":\"/archive\""
        ),
        "a row that deletes a conversation does not say that its argument is one of the \
         conversations: {opening:?}"
    );
    assert!(
        opening.contains(
            "\"from\":\"providers\",\"group\":\"model\",\"help\":\"delete one\",\
             \"label\":\"/provider rm <name>\",\"send\":\"/provider rm\""
        ),
        "the row that deletes a provider does not say that its argument is one of the providers, so \
         a page would have to tell the two lists apart by reading the command's name: {opening:?}"
    );
    // And the jobs row's argument is one of the jobs the page's own panel is showing, which is the
    // list a `/jobs stop` can actually take: the pids the panel drew a moment ago.
    assert!(
        opening.contains(
            "\"from\":\"jobs\",\"group\":\"work\",\"help\":\"end one of them\",\
             \"label\":\"/jobs stop <pid>\",\"send\":\"/jobs stop\""
        ),
        "the row that ends a job does not say that its argument is one of the run's jobs, so the \
         page would have to guess where a pid comes from: {opening:?}"
    );
    // And only those four: a `from` on a report row would have the page offer candidates for a
    // command that reads, and the count is what says the mark is a decision rather than a default.
    // The fourth arrived with `/jobs stop`, and the count moved deliberately rather than by making
    // the assertion a `>=`: a fifth mark added without thinking about it should fail here.
    //
    // The count is of *rows*, not of the string: `/say`'s address now carries a `from` of its own
    // inside its field object (`ArgFrom::Peers`, the list the picker offers), which is a different
    // question asked in the same word -- "which list does this argument come from" rather than "is this
    // row destructive". Counting occurrences would have made the two indistinguishable, so the rows are
    // parsed and the field-level keys are not counted.
    let marked = opening
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|json| serde_json::from_str::<serde_json::Value>(json).ok())
        .filter_map(|value| value.get("commands").and_then(|c| c.as_array()).cloned())
        .flatten()
        .filter(|row| row.get("from").is_some())
        .count();
    assert_eq!(
        marked, 4,
        "the frame marks a row as taking its argument from a list when it does not: {opening:?}"
    );
}

/// Every command the page may offer is one the terminal accepts.
///
/// The drift this catches is the whole reason the list is a table: a page that offers a button for
/// a command that has been renamed sends a line the REPL answers with `unknown command`, and
/// nothing in either renderer would notice, because the frame is built from the same table that
/// would be wrong. So the frame's own `send` strings are posted back through the route the page
/// uses and the answer is read: a page's menu is only as good as the terminal's dispatch.
///
/// Only the classes that are safe to run with no argument. A form would sit waiting for input and a
/// destructive one would delete something — which is what the class is *for*, and why the frame
/// carries it rather than letting the page guess from the name.
#[tokio::test]
async fn every_command_the_page_may_offer_is_one_the_terminal_takes() {
    let home = test_home("command-list-drift", "http://127.0.0.1:9/v1");
    let log = home.join("transcript.txt");
    let mut child = binary()
        .arg("--web")
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .stdin(std::process::Stdio::piped())
        .stdout(std::fs::File::create(&log).expect("transcript file"))
        .stderr(std::fs::File::create(home.join("stderr.txt")).expect("stderr file"))
        .spawn()
        .expect("failed to run flint");

    let (port, token) = port_and_token(&wait_for_url(&log));
    let mut watching = http_stream(port, "/events", &token);
    let opening = read_until(&mut watching, "\"type\":\"state\"}\n\n", 20);

    let mut checked = 0;
    for (label, send, _, class) in commands_in(&opening) {
        if class != "panel" && class != "button" {
            continue;
        }
        let posted = post_message(port, &token, &send);
        assert!(
            posted.starts_with("HTTP/1.1 202"),
            "`{label}` was refused at the route: {posted:?}"
        );
        let told = read_until(&mut watching, &format!("\"input\":\"{send}\""), 20);
        assert!(
            !told.contains("unknown command"),
            "the page is offered `{label}` as a {class} and the terminal does not have it, so a \
             button drawn from this frame would answer with a complaint: {told:?}"
        );
        if class == "button" {
            // A button's whole feedback is its answer, because pressing one leaves nothing else on
            // the page: an empty one would leave a reader unable to tell a press that worked from a
            // press that never arrived. Both of today's actions say a line.
            assert!(
                told.contains("\"text\":\"") && !told.contains("\"text\":\"\""),
                "`{label}` is offered as a button and answered with nothing: {told:?}"
            );
        }
        checked += 1;
    }

    drop(watching);
    drop(child.stdin.take());
    let exited = wait_for_exit(&mut child, 20);
    let _ = std::fs::remove_dir_all(&home);

    assert!(exited, "flint did not exit");
    assert!(
        checked >= 8,
        "only {checked} commands were checked, so the frame is missing the reports and actions \
         this class is made of"
    );
}


