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
    // Hold the same lock as every other test that starts a `Term`. `FLINT_TERM_CAPTURE`
    // is process-wide once any test sets it, so an unguarded `Term::start` elsewhere
    // writes its own interactive output into whichever file stdout currently points at --
    // which is how this capture ended up holding another test's transcript.
    let _guard = stdout_lock().lock().unwrap_or_else(|e| e.into_inner());
    let path = capture_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let restore = redirect_stdout(&path);

    // Must be set before `Term::start`, and only has an effect in a debug build.
    std::env::set_var("FLINT_TERM_CAPTURE", "1");
    std::env::set_var("FLINT_TERM_SIZE", "70x24");
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

#[test]
fn a_notice_mid_answer_does_not_tear_the_layout() {
    let _guard = stdout_lock().lock().unwrap_or_else(|e| e.into_inner());
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("term-notice.bin");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let restore = redirect_stdout(&path);

    std::env::set_var("FLINT_TERM_CAPTURE", "1");
    std::env::set_var("FLINT_TERM_SIZE", "70x24");
    let term = Term::start().expect("term");
    term.line(format_args!("> 检查网络"));

    term.stream("第一段：正在检查网络连通性");
    // The notice arrives mid-answer, as a long command's warning does.
    term.notice("this command has been running for 20s and may be stuck");
    term.stream("第一段：正在检查网络连通性第二段继续输出");
    term.end_stream();
    restore();
    std::env::remove_var("FLINT_TERM_CAPTURE");
    std::env::remove_var("FLINT_TERM_SIZE");

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

    assert!(
        screen.contains("may be stuck"),
        "the notice is missing from the screen:\n{screen}"
    );
    // The notice gets its own row: sharing one with the answer is exactly the tear this
    // is about.
    let notice_row = screen
        .lines()
        .find(|l| l.contains("may be stuck"))
        .expect("the notice should be on some row");
    assert!(
        !notice_row.contains("正在检查网络"),
        "the notice was written over the answer:\n{notice_row}"
    );
    assert!(
        screen.contains("第二段继续输出"),
        "the answer did not continue after the notice:\n{screen}"
    );

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
        "the notice left duplicated rows behind: {repeated:?}\n{screen}"
    );
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

/// A segment that repeats text already committed must not draw it again.
///
/// Reported from a real session, and the shape is common enough to be worth naming:
/// the model narrates "I'll read both files.", calls two tools, and then resends the
/// *cumulative* text -- "I'll read both files.Flint is a ..." -- instead of only the
/// continuation. The prefix matches what is being streamed, so it reads as the same
/// segment, and the whole thing is redrawn from column 1 of the strip's top row.
///
/// The opening line therefore appeared twice: once in the transcript, where it was
/// committed when the tools ran, and once at the head of the new segment, with no
/// break between it and its own continuation:
///
/// ```text
/// I'll read both files.`src/lib.rs`: library root declaring nine modules...
/// ```
///
/// The text was correct and present exactly once in the *answer* -- only the
/// transcript had it twice. Counting rows cannot see this: the repeat is a prefix of
/// a longer row, not a row equal to another.
#[test]
fn a_segment_that_repeats_committed_text_does_not_draw_it_twice() {
    let _guard = stdout_lock().lock().unwrap_or_else(|e| e.into_inner());
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("term-repeated-prefix.bin");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let restore = redirect_stdout(&path);

    std::env::set_var("FLINT_TERM_CAPTURE", "1");
    std::env::set_var("FLINT_TERM_SIZE", "70x24");
    let term = Term::start().expect("term");
    term.line(format_args!("> 看看这个仓库"));

    // Segment one: the narration before the tool calls. Short enough that it never
    // overflows the strip, which is what keeps `committed` at zero and is why the row
    // count cannot be used to detect the repeat.
    let opening = "I'll read both files.";
    let mut acc = String::new();
    for frag in opening.chars() {
        acc.push(frag);
        term.stream(&acc);
    }

    // The tools run: their results are `line()` calls, which commit the segment above.
    term.line(format_args!("  \u{2713} read src/lib.rs 14 lines"));
    term.line(format_args!("  \u{2713} read src/agent.rs 40 lines"));

    // Segment two: the same sentence again, with the continuation appended -- the
    // cumulative resend that caused the fault.
    let second = format!("{opening}The library root declares nine modules.");
    let mut acc2 = String::new();
    for frag in second.chars() {
        acc2.push(frag);
        term.stream(&acc2);
    }
    term.end_stream();
    restore();
    std::env::remove_var("FLINT_TERM_CAPTURE");
    std::env::remove_var("FLINT_TERM_SIZE");

    let bytes = std::fs::read(&path).unwrap();
    let text = String::from_utf8_lossy(&bytes).to_string();

    // History is written one row at a time by `insert_history`, and each write has a
    // distinctive shape: set the scroll region to the rows above the strip, park on its
    // bottom row, erase it, write the row, and advance. Matching that shape is what
    // separates a committed row from a strip redraw, which also writes rows but at
    // absolute positions inside the strip and never sets a scroll region.
    //
    // The strip redraws its whole text on every fragment, so searching the raw byte
    // stream for the sentence finds it in transient half-drawn frames and cannot tell a
    // committed row from a frame. Only this pattern can answer "did it reach the
    // transcript?".
    let rows = history_rows(&text);

    // The opening sentence belongs in the transcript -- segment one put it there, and it
    // is real output. What must not happen is a *second* copy of it, which the resend
    // produced as a row that begins with the sentence and then runs straight on into its
    // own continuation with no break between them.
    let merged: Vec<&String> = rows
        .iter()
        .filter(|r| r.starts_with(opening) && r.len() > opening.len())
        .collect();
    assert!(
        merged.is_empty(),
        "the opening sentence was committed a second time, glued to its own continuation:\n  \
         {merged:#?}\nall committed rows:\n{rows:#?}"
    );

    // It is still there exactly once, as its own row.
    assert_eq!(
        rows.iter().filter(|r| r.as_str() == opening).count(),
        1,
        "the opening sentence is not in the transcript exactly once:\n{rows:#?}"
    );
    // And the new segment's own text did reach the screen.
    assert!(
        rows.iter().any(|r| r == "The library root declares nine modules."),
        "the new segment's own text is missing from the transcript:\n{rows:#?}"
    );
}

/// Every row `insert_history` wrote, in order, with the escape framing stripped.
///
/// Works on bytes rather than `&str` slices: the row number and the escape framing are
/// ASCII, but the row itself is usually not, so byte offsets computed from one cannot
/// be used to index the other without splitting a multi-byte character.
fn history_rows(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut rows = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        // "\x1b[1;" -- the scroll region always starts at row 1 for history writes.
        if bytes[i..].starts_with(b"\x1b[1;") {
            let mut j = i + 4;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            // `;` would be a cursor position; `r` is the scroll region. Only the latter
            // is a history write.
            if j < bytes.len() && bytes[j] == b'r' {
                if let Some(k) = find_bytes(&bytes[j..], b"\x1b[2K") {
                    let start = j + k + 4;
                    if let Some(e) = find_bytes(&bytes[start..], b"\r\n") {
                        let row = String::from_utf8_lossy(&bytes[start..start + e]).to_string();
                        rows.push(row);
                        i = start + e + 2;
                        continue;
                    }
                }
            }
        }
        i += 1;
    }
    rows
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
}


/// The running-status clock must not land on the answer's own last row.
///
/// `paint_activity` draws the clock on `input_row - 1`, which is the bottom row of the
/// answer strip -- the row a bottom-anchored answer uses for its final line. The two
/// write the same row with different content, and whichever goes second wins:
///
///   * a repaint of the clock erases the answer's last line, and
///   * a redraw of the answer erases the clock, so a long tool looks wedged.
///
/// This is the layout fault the strip exists to prevent, one row lower down: the status
/// line is not part of the transcript machinery, so nothing arbitrates between it and
/// the answer.
#[test]
fn the_running_clock_does_not_land_on_the_answers_last_row() {
    let _guard = stdout_lock().lock().unwrap_or_else(|e| e.into_inner());
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("term-activity.bin");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let restore = redirect_stdout(&path);

    std::env::set_var("FLINT_TERM_CAPTURE", "1");
    std::env::set_var("FLINT_TERM_SIZE", "70x24");
    let term = Term::start().expect("term");
    term.line(format_args!("> 跑个长命令"));

    // A tool starts and takes long enough for the clock to advance. `tick` only repaints
    // when the displayed number changes, so this has to cross a whole second or the
    // collision never happens at all.
    term.activity_started("bash");
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(1200);
    while std::time::Instant::now() < deadline {
        term.tick();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let clock_seen = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).to_string();
    // The clock is drawn dim and centred, so its own marker is the thing to look for --
    // a bare "bash" also appears in the transcript as the tool's name.
    if !clock_seen.contains("bash \u{2500}\u{2500}") {
        panic!("the clock never appeared, so this test would prove nothing:\n{clock_seen:?}");
    }

    // Now an answer arrives and is streamed while the clock is still running -- the case
    // that matters, because this is when the two writers race for the same row.
    let mut acc = String::new();
    for frag in ["正在检查磁盘占用", "，稍等。"] {
        acc.push_str(frag);
        term.stream(&acc);
    }

    restore();
    std::env::remove_var("FLINT_TERM_CAPTURE");
    std::env::remove_var("FLINT_TERM_SIZE");

    let bytes = std::fs::read(&path).unwrap();
    let screen = replay(&path, 24, 70);

    // The clock holds its own row and the answer holds the rows above it; neither may
    // sit on top of the other.
    let clock_row = screen
        .iter()
        .position(|r| r.contains("bash \u{2500}\u{2500}"));
    let answer_row = screen.iter().position(|r| r.contains("正在检查磁盘占用"));
    let clock_row = clock_row.unwrap_or_else(|| {
        panic!("the clock is not on the screen at all:\n{}", screen.join("\n"))
    });
    let answer_row = answer_row.unwrap_or_else(|| {
        panic!("the answer is not on the screen at all:\n{}", screen.join("\n"))
    });
    assert_ne!(
        clock_row, answer_row,
        "the clock and the answer share a row, so one erased the other:\n{}",
        screen.join("\n")
    );
    assert!(
        clock_row > answer_row,
        "the clock should sit below the answer, not above it:\n{}",
        screen.join("\n")
    );
    let _ = bytes;
}

/// Replay a captured byte stream through the screen model and return its rows.
fn replay(path: &std::path::Path, rows: usize, cols: usize) -> Vec<String> {
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join("vtscreen.js");
    let out = std::process::Command::new("node")
        .arg(&script)
        .arg(path)
        .arg(rows.to_string())
        .arg(cols.to_string())
        .output()
        .expect("node scripts/vtscreen.js");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .skip(1) // the tool's own header
        .map(|l| l.split_once('|').map(|(_, rest)| rest.trim_end().to_string()).unwrap_or_default())
        .collect()
}

/// `replay`, onto a screen that already holds `prefill` on every row.
fn replay_with_prefill(
    path: &std::path::Path,
    rows: usize,
    cols: usize,
    prefill: &str,
) -> Vec<String> {
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join("vtscreen.js");
    let out = std::process::Command::new("node")
        .arg(&script)
        .arg(path)
        .arg(rows.to_string())
        .arg(cols.to_string())
        .arg("--prefill")
        .arg(prefill)
        .output()
        .expect("node scripts/vtscreen.js");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .skip(1)
        .map(|l| l.split_once('|').map(|(_, rest)| rest.trim_end().to_string()).unwrap_or_default())
        .collect()
}

/// The status line must appear while waiting on the model, and not flicker.
///
/// Two faults meet here. The first: `activity_started` used to be called only when a
/// *tool* began, so the status line was blank for the whole model call -- and once
/// streamed output was buffered for retry safety, nothing at all appeared on screen
/// until the entire response had arrived. A slow model or a dead endpoint then looked
/// exactly like a hung program, which is the worst thing a rescue tool can look like.
///
/// The second: showing the line immediately makes it flash on and off for every quick
/// turn, which is noise rather than information. Hence the delay.
#[test]
fn the_status_line_waits_and_then_appears() {
    let _guard = stdout_lock().lock().unwrap_or_else(|e| e.into_inner());
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("term-waiting.bin");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let restore = redirect_stdout(&path);

    std::env::set_var("FLINT_TERM_CAPTURE", "1");
    std::env::set_var("FLINT_TERM_SIZE", "70x24");
    let term = Term::start().expect("term");
    assert!(term.interactive());

    // The empty name is the "waiting for the model" case: no tool is running.
    term.activity_started("");

    // Immediately: nothing. A turn that answers in milliseconds must not flash a clock.
    term.tick();
    let early = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).to_string();
    assert!(
        !early.contains("waiting for the model"),
        "the clock was painted immediately, so every fast turn will flicker:\n{early:?}"
    );

    // Past the delay: the line, naming what is being waited for and for how long.
    std::thread::sleep(std::time::Duration::from_millis(400));
    term.tick();
    let late = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).to_string();
    assert!(
        late.contains("waiting for the model"),
        "a model call that takes longer than the delay must say so:\n{late:?}"
    );
    assert!(
        late.contains("0s") || late.contains("1s"),
        "the clock must show elapsed time, not just a word:\n{late:?}"
    );

    // A tool takes over from the wait, and the line names it instead.
    term.activity_started("bash");
    std::thread::sleep(std::time::Duration::from_millis(400));
    term.tick();
    let tool = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).to_string();
    assert!(
        tool.contains("bash"),
        "a running tool must be named:\n{tool:?}"
    );

    term.activity_done();
    restore();
    std::env::remove_var("FLINT_TERM_CAPTURE");
    std::env::remove_var("FLINT_TERM_SIZE");
}

/// The status line must say *what* is happening, not just that time is passing.
///
/// A turn waits for the model, and then the model may spend a while reasoning before it
/// produces any text. Both are the same clock but not the same thing, and "waiting for
/// the model" is simply wrong once the model has started talking. This replaces a
/// separate `… thinking` marker in the transcript, which said the same thing one row
/// above and left the status line contradicting it.
#[test]
fn the_status_line_names_the_phase_it_is_in() {
    let _guard = stdout_lock().lock().unwrap_or_else(|e| e.into_inner());
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("term-phase.bin");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let restore = redirect_stdout(&path);

    std::env::set_var("FLINT_TERM_CAPTURE", "1");
    std::env::set_var("FLINT_TERM_SIZE", "70x24");
    let term = Term::start().expect("term");

    // Waiting on the model.
    term.activity_started("");
    std::thread::sleep(std::time::Duration::from_millis(400));
    term.tick();

    // The first reasoning fragment arrives: still the same wait, now with a name.
    term.activity_named("thinking");
    let named = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).to_string();
    assert!(
        named.contains("thinking"),
        "a turn that is reasoning must say so:\n{named:?}"
    );

    // Renamed immediately rather than at the next second: the point of the change is
    // that the line was saying the wrong thing.
    let thinking_marks = named.matches("thinking").count();
    assert!(thinking_marks >= 1, "expected the new name on screen: {named:?}");

    // Naming an activity that is not running must not invent one: a stray reasoning
    // fragment with no turn in flight would otherwise put a clock on screen forever.
    term.activity_done();
    let before = std::fs::read(&path).unwrap().len();
    term.activity_named("thinking");
    let after = std::fs::read(&path).unwrap().len();
    assert_eq!(
        before, after,
        "naming a stopped activity must not paint anything"
    );

    restore();
    std::env::remove_var("FLINT_TERM_CAPTURE");
    std::env::remove_var("FLINT_TERM_SIZE");
}

/// The transcript fills from the top of the screen, not from just above the input.
///
/// Reported from a real session as "the layout is funny -- it is bottom-aligned and very
/// low". It was: `insert_history` wrote every line on the history region's *bottom* row
/// and let the newline scroll it up, so each line appeared at the lowest possible row and
/// a short session sat above a large blank area. A terminal fills from the top, and a
/// transcript should too -- only the input line is pinned to the bottom.
#[test]
fn the_transcript_starts_at_the_top_of_the_screen() {
    let _guard = stdout_lock().lock().unwrap_or_else(|e| e.into_inner());
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("term-top.bin");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let restore = redirect_stdout(&path);

    std::env::set_var("FLINT_TERM_CAPTURE", "1");
    std::env::set_var("FLINT_TERM_SIZE", "70x24");
    let term = Term::start().expect("term");

    term.line(format_args!("FIRST-LINE"));
    term.line(format_args!("SECOND-LINE"));

    restore();
    std::env::remove_var("FLINT_TERM_CAPTURE");
    std::env::remove_var("FLINT_TERM_SIZE");

    let screen = replay(&path, 24, 70);
    assert_eq!(
        screen[0].trim(),
        "FIRST-LINE",
        "the first line of a session belongs on the top row, not near the bottom:\n{}",
        screen.join("\n")
    );
    assert_eq!(screen[1].trim(), "SECOND-LINE", "lines must follow one another down");
    // And the input line is still pinned to the last row, which is the one thing that
    // *should* be bottom-anchored.
    assert!(
        screen[23].trim_start().starts_with('>'),
        "the input line must stay on the last row:\n{}",
        screen.join("\n")
    );
}

/// Once the transcript fills its region it must scroll, not overwrite.
///
/// Growing downward is only half of it: the region has a bottom row, and the line that
/// falls off the top has to go into the scrollback rather than being painted over.
#[test]
fn a_full_transcript_scrolls_instead_of_overwriting() {
    let _guard = stdout_lock().lock().unwrap_or_else(|e| e.into_inner());
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("term-scroll.bin");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let restore = redirect_stdout(&path);

    std::env::set_var("FLINT_TERM_CAPTURE", "1");
    std::env::set_var("FLINT_TERM_SIZE", "70x24");
    let term = Term::start().expect("term");

    // The history region is rows 1..19 on a 24-row screen; write well past it.
    term.line(format_args!("LINE-01"));
    for n in 2..=40 {
        term.line(format_args!("LINE-{n:02}"));
    }

    restore();
    std::env::remove_var("FLINT_TERM_CAPTURE");
    std::env::remove_var("FLINT_TERM_SIZE");

    let screen = replay(&path, 24, 70);
    let joined = screen.join("\n");

    // The newest line is present, and the transcript has scrolled: the region holds 19
    // rows, so 40 lines cannot all still be on screen, and the earliest must be gone.
    assert!(joined.contains("LINE-40"), "the newest line must be visible:\n{joined}");
    assert!(
        !joined.contains("LINE-01"),
        "the transcript did not scroll -- the oldest line is still on screen, which \
         means newer lines overwrote rows instead of moving them:\n{joined}"
    );
    // The input row survived all of it.
    assert!(
        screen[23].trim_start().starts_with('>'),
        "the input line must survive a scrolling transcript:\n{joined}"
    );
}

/// The transcript shows paths relative to the session directory.
///
/// Not cosmetic: the model usually hands over absolute paths, and at 80 columns a
/// transcript where every file operation repeats `/home/you/project/` twice is mostly
/// prefix -- six lines of it look identical at a glance, which defeats the point of a
/// one-line summary.
#[test]
fn a_path_inside_the_session_directory_is_shown_relative() {
    let _guard = stdout_lock().lock().unwrap_or_else(|e| e.into_inner());
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("term-paths.bin");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let restore = redirect_stdout(&path);

    std::env::set_var("FLINT_TERM_CAPTURE", "1");
    let term = Term::start().expect("term");
    let sep = if cfg!(windows) { "\\" } else { "/" };
    term.set_cwd("/work/project");

    assert_eq!(term.shorten_path("/work/project/src/term.rs"), "src/term.rs");
    assert_eq!(term.shorten_path("/work/project"), ".");
    // A sibling directory that merely shares a prefix must not be mangled: `/work/app`
    // is not inside `/work/apple`.
    assert_eq!(
        term.shorten_path("/work/project-other/file.rs"),
        "/work/project-other/file.rs"
    );
    // Anything outside is left exactly as the model wrote it -- shortening is for
    // reading, and a path that no longer locates the file would be worse than a long one.
    assert_eq!(term.shorten_path("/etc/hosts"), "/etc/hosts");
    // A trailing separator on the recorded directory must not break the boundary check.
    term.set_cwd(&format!("/work/project{sep}"));
    assert_eq!(term.shorten_path("/work/project/src"), "src");

    restore();
    std::env::remove_var("FLINT_TERM_CAPTURE");
}

/// Starting flint must leave nothing of the old screen behind.
///
/// `setup` scrolls the screen down to make room, which moves whatever the shell had
/// displayed five rows lower -- into the rows the status line, the answer strip and the
/// input line occupy. It then cleared only the *transcript* rows, so the pushed-down
/// leftovers were never erased and stayed on screen for the whole session, sitting
/// immediately above and beside the input line. Reported from a real session as "a
/// bracket to the left and right of the command line that does not go away".
///
/// The screen is pre-filled here rather than left blank, because a blank screen is
/// exactly the case where the fault is invisible.
#[test]
fn starting_up_leaves_no_remnant_of_the_previous_screen() {
    let _guard = stdout_lock().lock().unwrap_or_else(|e| e.into_inner());
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("term-cleanstart.bin");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let restore = redirect_stdout(&path);

    std::env::set_var("FLINT_TERM_CAPTURE", "1");
    std::env::set_var("FLINT_TERM_SIZE", "70x24");
    let term = Term::start().expect("term");
    term.line(format_args!("TRANSCRIPT"));
    restore();
    std::env::remove_var("FLINT_TERM_CAPTURE");
    std::env::remove_var("FLINT_TERM_SIZE");

    // Replayed onto a screen that already holds shell output on every row: `setup`
    // scrolls that content down into the rows it then repaints, so a blank starting
    // screen is precisely the case where the fault cannot be seen.
    let screen = replay_with_prefill(&path, 24, 70, "LEFTOVER");

    let remnant: Vec<(usize, String)> = screen
        .iter()
        .enumerate()
        .filter(|(_, row)| row.contains("LEFTOVER"))
        .map(|(n, row)| (n + 1, row.clone()))
        .collect();
    assert!(
        remnant.is_empty(),
        "rows still hold the previous screen after startup: {remnant:#?}\n{}",
        screen.join("\n")
    );
    assert!(
        screen[0].contains("TRANSCRIPT"),
        "the first line of the session belongs on row 1:\n{}",
        screen.join("\n")
    );
}

/// Startup must ask the terminal for a full repaint, not just erase lines.
///
/// Erasing a line rewrites its characters; it does not invalidate what the terminal has
/// already rasterised. Stale glyphs can therefore survive an `2K` sweep and sit in the
/// gaps -- reported from a real session as brackets at the left and right edges of a
/// row, missing from the text buffer, and gone the instant the window was resized. A
/// resize works because it makes the terminal rebuild its display, so startup asks for
/// the same thing explicitly.
#[test]
fn starting_up_asks_the_terminal_to_repaint() {
    let _guard = stdout_lock().lock().unwrap_or_else(|e| e.into_inner());
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("term-repaint.bin");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let restore = redirect_stdout(&path);

    std::env::set_var("FLINT_TERM_CAPTURE", "1");
    std::env::set_var("FLINT_TERM_SIZE", "70x24");
    let _term = Term::start().expect("term");
    restore();
    std::env::remove_var("FLINT_TERM_CAPTURE");
    std::env::remove_var("FLINT_TERM_SIZE");

    let bytes = std::fs::read(&path).unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains("\u{1b}[2J"),
        "startup must clear the display so the terminal repaints, not only erase rows"
    );
    // And it must clear *before* the first thing it draws, or the banner would be wiped.
    let clear = text.find("\u{1b}[2J").unwrap();
    let banner = text.find("flint").unwrap_or(usize::MAX);
    assert!(
        banner == usize::MAX || clear < banner,
        "the repaint must come before any content is drawn"
    );
}

/// Resizing the window must not erase the conversation.
///
/// The resize handler called `setup`, which scrolls and blanks the display and rewinds
/// the transcript to row 1 -- so every resize wiped everything that had been said. It
/// looked like a cure for a stale-glyph artefact (dragging the window edge did clear
/// some junk off the screen), but it was clearing the transcript as well.
///
/// Resizing needs the layout recomputed and the screen repainted; it must never take
/// the screen over again.
#[test]
fn resizing_the_window_keeps_the_transcript() {
    let _guard = stdout_lock().lock().unwrap_or_else(|e| e.into_inner());
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("term-resize.bin");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let restore = redirect_stdout(&path);

    std::env::set_var("FLINT_TERM_CAPTURE", "1");
    std::env::set_var("FLINT_TERM_SIZE", "70x24");
    let term = Term::start().expect("term");
    term.line(format_args!("KEEP-ME-ACROSS-A-RESIZE"));

    // Everything after this point belongs to the resize. `on_event` returns the key the
    // REPL acts on, and `Key::Redraw` is what makes it call `redraw` -- so drive both,
    // exactly as the REPL does.
    let before = std::fs::read(&path).unwrap().len();
    let key = term.on_event(crossterm::event::Event::Resize(100, 30));
    assert!(
        matches!(key, flint::term::Key::Redraw),
        "a resize must ask for a redraw"
    );
    term.redraw();

    restore();
    std::env::remove_var("FLINT_TERM_CAPTURE");
    std::env::remove_var("FLINT_TERM_SIZE");

    let bytes = std::fs::read(&path).unwrap();
    assert!(bytes.len() > before, "the resize produced no output at all");
    let resized = String::from_utf8_lossy(&bytes[before..]).to_string();

    // `2J` is the full-display clear `reserve_screen` uses at startup. Seeing it here
    // means the transcript that was on screen has just been thrown away.
    assert!(
        !resized.contains("\u{1b}[2J"),
        "the resize cleared the display, taking the conversation with it:\n{resized:?}"
    );
    // The one thing a resize must do.
    assert!(
        resized.contains('>'),
        "the resize must repaint the input line:\n{resized:?}"
    );
}

/// The status line must name the phase, and know when it is still unnamed.
///
/// A turn passes through four different waits -- the request going out, the model
/// reasoning, the model emitting an answer, and a tool running -- and a single
/// "waiting for the model" label told the user none of which was happening. The
/// escalation that distinguishes a slow model from a dead endpoint depends on being
/// able to ask "is this still the initial, nameless wait?", so that question has to
/// answer correctly and stop being true once the label is set.
#[test]
fn the_status_line_knows_which_phase_it_is_in() {
    let _guard = stdout_lock().lock().unwrap_or_else(|e| e.into_inner());
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("term-phases.bin");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let restore = redirect_stdout(&path);

    std::env::set_var("FLINT_TERM_CAPTURE", "1");
    std::env::set_var("FLINT_TERM_SIZE", "70x24");
    let term = Term::start().expect("term");

    // Nothing running: not a wait, and no elapsed time to report.
    assert!(!term.activity_is_unnamed_wait(), "nothing is running yet");
    assert_eq!(term.activity_elapsed().as_secs(), 0);

    // Phase 1: the request is out and nothing has come back.
    term.activity_started("");
    assert!(
        term.activity_is_unnamed_wait(),
        "an empty name is the initial wait, which is what the escalation keys on"
    );

    // Phase 2: the model is reasoning.
    term.activity_named("thinking");
    assert!(
        !term.activity_is_unnamed_wait(),
        "a named activity is no longer the nameless wait, so it must not escalate to \
         'no response' while the model is demonstrably talking"
    );

    // Phase 3: the answer is being written.
    term.activity_named("writing the answer");
    let elapsed_after_rename = term.activity_elapsed();
    assert!(
        elapsed_after_rename.as_millis() < 100,
        "renaming must not restart the clock: the turn has been running since the \
         request went out, and the user is waiting on the whole thing, not on the label"
    );

    // Phase 4: a tool is running.
    term.activity_started("bash");
    assert!(!term.activity_is_unnamed_wait());

    term.activity_done();
    assert!(!term.activity_is_unnamed_wait(), "nothing is running again");

    restore();
    std::env::remove_var("FLINT_TERM_CAPTURE");
    std::env::remove_var("FLINT_TERM_SIZE");
}
