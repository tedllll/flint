// The only test that exercises the *real* interactive byte stream.
//
// `Term`'s interactive branches are normally unreachable from a test: they need a
// terminal, and crossterm talks to the console rather than to stdin. A debug build
// honours `FLINT_TERM_CAPTURE`, so this test points stdout at a file, drives `Term`
// the way `run_turn` does, and then hands the bytes to the replay checker in
// scripts/term-layout-test.js.
//
// That closes the gap between the hand-written model in scripts/ and the Rust code
// it is supposed to mirror: if the two ever disagree, this test fails.
#![cfg(debug_assertions)]

use flint::term::Term;
use std::io::Write;

/// Redirect the process's stdout to `path` and return a guard that puts it back.
///
/// `libc::dup`/`dup2` are the portable way to do this; flushing first matters
/// because Rust buffers stdout and would otherwise swallow anything pending.
#[cfg(unix)]
fn redirect_stdout(path: &std::path::Path) -> Box<dyn FnOnce()> {
    use std::os::unix::io::AsRawFd;
    let file = std::fs::File::create(path).unwrap();
    let saved = unsafe { libc::dup(1) };
    std::io::stdout().flush().unwrap();
    assert!(saved >= 0);
    assert!(unsafe { libc::dup2(file.as_raw_fd(), 1) } >= 0);
    Box::new(move || {
        std::io::stdout().flush().unwrap();
        unsafe { libc::dup2(saved, 1) };
        unsafe { libc::close(saved) };
    })
}

#[cfg(windows)]
fn redirect_stdout(path: &std::path::Path) -> Box<dyn FnOnce()> {
    use std::os::windows::io::AsRawHandle;
    let file = std::fs::File::create(path).unwrap();
    std::io::stdout().flush().unwrap();
    let saved = unsafe { libc::dup(1) };
    assert!(saved >= 0, "dup failed");
    // The CRT's file descriptor has to be backed by a HANDLE, so the Rust File is
    // opened onto a fresh handle for fd 1 rather than reusing the one it already
    // has; `forget` keeps it from closing a handle fd 1 now owns.
    let fd = unsafe { libc::open_osfhandle(file.as_raw_handle() as isize, 0) };
    assert!(fd >= 0, "open_osfhandle failed");
    std::mem::forget(file);
    assert!(unsafe { libc::dup2(fd, 1) } >= 0, "dup2 failed");
    Box::new(move || {
        std::io::stdout().flush().unwrap();
        unsafe { libc::dup2(saved, 1) };
        unsafe { libc::close(saved) };
    })
}

/// Serialises the tests that move stdout.
///
/// `dup2` on file descriptor 1 is process-wide, so two of these running at once
/// point it at each other's files and every assertion reads someone else's output.
/// The lock is what makes `redirect_stdout` usable from more than one test.
fn stdout_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

/// Path the capture is written to, read back by scripts/term-layout-test.js.
fn capture_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("term-capture.bin")
}

#[test]
fn interactive_layout_matches_the_replay_model() {
    let _guard = stdout_lock().lock().unwrap_or_else(|e| e.into_inner());
    let path = capture_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let restore = redirect_stdout(&path);

    // Must be set before `Term::start`, and only has an effect in a debug build.
    std::env::set_var("FLINT_TERM_CAPTURE", "1");
    let term = Term::start().expect("term");
    assert!(term.interactive(), "capture override did not engage");

    // A turn the way the REPL runs it: banner, the echo of the question, the single
    // thinking marker, a tool result, then a short final answer. Each of those was a
    // real bug at some point, so they are all in the one capture.
    term.line(format_args!("flint v0.1.0  deepseek/deepseek-flash"));
    term.blank();
    term.line(format_args!("> 你好，帮我看看磁盘"));
    term.line(format_args!("\u{2026} thinking"));
    term.line(format_args!("  ✓ TOOL_ROUND_OK"));
    let mut answer = String::new();
    for frag in ["磁盘", "占用", "正常", "。"] {
        answer.push_str(frag);
        term.stream(&answer);
    }
    term.end_stream();

    // Reading the file back needs the redirect gone, or the read is fine but the
    // assertion messages get mixed into the capture.
    restore();
    std::env::remove_var("FLINT_TERM_CAPTURE");

    let bytes = std::fs::read(&path).unwrap();
    assert!(!bytes.is_empty(), "nothing was captured");
    assert!(
        bytes.starts_with(b"\x1b["),
        "capture does not look like the interactive stream: {:?}",
        &bytes[..bytes.len().min(40)]
    );
}

#[test]
fn the_capture_helper_writes_where_the_replay_expects() {
    // Cheap guard: if the path changes, the replay script silently checks a stale
    // file, which is worse than failing.
    assert!(capture_path().ends_with("target/term-capture.bin"));
}

/// A second, different answer in the same turn must replace the first, not stack on it.
///
/// Reported from a real session: the model narrated "I'll run a few network
/// diagnostics" before each round of tool calls, and that sentence stayed on screen
/// under the next round's text, so it was visible once per round. The reasoning and
/// the answer are separate streamed segments, and `line()` only writes *history* -- it
/// never touches the strip -- so without an explicit clear the old segment simply
/// sits there while the new one draws over and past it.
#[test]
fn a_new_streamed_segment_replaces_the_previous_one() {
    let _guard = stdout_lock().lock().unwrap_or_else(|e| e.into_inner());
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("term-segments.bin");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let restore = redirect_stdout(&path);

    std::env::set_var("FLINT_TERM_CAPTURE", "1");
    // Pin the size, because the replay below has to use the same one. A capture made
    // at whatever width the harness reports wraps in different places than a replay at
    // 70 columns, which manufactures failures that are not in the code.
    std::env::set_var("FLINT_TERM_SIZE", "70x24");
    let term = Term::start().expect("term");
    term.line(format_args!("> 测试连通性"));

    // Segment one, long enough to occupy several strip rows.
    let first = "我先跑几条网络诊断命令，看看回环、DNS 和外网是否都通。";
    let mut acc = String::new();
    for frag in first.chars() {
        acc.push(frag);
        term.stream(&acc);
    }
    // A tool result goes to history between segments, as it does in a real turn.
    term.line(format_args!("  \u{2713} bash 9 lines"));

    // Segment two: unrelated text, as the model's next narration would be.
    let second = "回环正常，DNS 也通。";
    let mut acc2 = String::new();
    for frag in second.chars() {
        acc2.push(frag);
        term.stream(&acc2);
    }
    term.end_stream();
    restore();
    std::env::remove_var("FLINT_TERM_CAPTURE");
    std::env::remove_var("FLINT_TERM_SIZE");

    // Replay both this and the layout capture, so the model is checked too.
    let bytes = std::fs::read(&path).unwrap();
    assert!(!bytes.is_empty(), "nothing was captured");
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join("vtscreen.js");
    let out = std::process::Command::new("node")
        .arg(&script)
        .arg(&path)
        .arg("24")
        .arg("70")
        .output()
        .expect("node scripts/vtscreen.js");
    let screen = String::from_utf8_lossy(&out.stdout).to_string();

    // The screen shows segment two; segment one has been committed to the transcript
    // above, which is what the screen *is*, so it must appear exactly once there.
    assert!(screen.contains("回环正常"), "the new segment is missing:\n{screen}");
    let first_seen = screen.matches("我先跑几条网络诊断").count();
    assert_eq!(
        first_seen, 1,
        "the previous segment should appear once, not once per round:\n{screen}"
    );

    // The structural version of the same claim, which is what actually went wrong on a
    // long multi-round turn: no two consecutive rows of the transcript may be identical.
    // Counting occurrences in the text cannot catch it, because the duplicated rows are
    // the paragraph re-wrapped -- the same characters in the same order, not a second
    // copy appended after the first.
    let rows: Vec<&str> = screen
        .lines()
        .filter_map(|l| l.split_once('|').map(|(_, rest)| rest.trim_end()))
        .collect();
    let repeated: Vec<&str> = rows
        .windows(2)
        .filter(|w| !w[0].is_empty() && w[0] == w[1])
        .map(|w| w[0])
        .collect();
    assert!(
        repeated.is_empty(),
        "the transcript wrote these rows twice in a row: {repeated:?}\n{screen}"
    );
}
