// A terminal that goes away must take the run with it.
//
// When a pty's master is closed -- a terminal emulator that crashes, a `close(master)` from the
// other end, an ssh session whose SIGHUP never arrived -- flint used to spin at 100% of a core for
// ever and never exit. The spin is not in flint: `crossterm::event::read()` is `try_read(None)`,
// whose loop condition is always true, and a hung-up descriptor reports `POLLHUP` without `POLLIN`,
// which none of its three branches consumes -- so `poll` returns at once, nothing is read, and it
// polls again (crossterm 0.29, `event/source/unix/tty.rs`). Nothing in flint can see that from
// inside the call, which is why the run is watched from *outside* it.
//
// Unix only, and it needs a real pty, so this is the one suite that does not run on Windows: there,
// crossterm's console read returns the error itself rather than spinning, and the case was measured
// on Unix in the first place.
#![cfg(unix)]

use std::os::unix::io::FromRawFd;

/// A `FLINT_HOME` of this test's own, with a provider that is never reached.
///
/// The run under test says nothing and asks nothing: the pty opens, the banner is drawn, and the
/// process waits for a key. No model is called, so the endpoint only has to exist as a value.
fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("flint-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sessions")).expect("temp home");
    std::fs::write(
        dir.join("config.toml"),
        "default_provider = \"stub\"\n\n\
         [[providers]]\n\
         name = \"stub\"\n\
         base_url = \"http://127.0.0.1:1/v1\"\n\
         model = \"stub-model\"\n\
         api_key = \"not-a-real-key\"\n",
    )
    .expect("failed to write the test config");
    dir
}

/// Everything the master has to give right now, so the test can look at the screen without waiting.
fn drain(fd: libc::c_int) -> String {
    // Non-blocking, or a read with nothing behind it waits for the child's next byte.
    unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) };
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        match n {
            1.. => out.extend_from_slice(&buf[..n as usize]),
            _ => break,
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

/// The same text with the terminal's escape sequences taken out.
///
/// The banner is coloured per piece (`flint` bold, `v0.1.0` dim, and so on), so the words are only
/// contiguous on screen, never in the bytes a pty hands back. Learned from the first run of this
/// test on CI, which failed the banner check while the banner was right there in the capture,
/// separated by `ESC [ 1 m`.
fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        // A CSI sequence: `ESC [` then parameters and intermediates, ending in a letter.
        if chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        }
    }
    out
}

/// Seconds of CPU this process has burned, for the failure message.
///
/// A run that hangs and a run that spins look the same to a stopwatch and completely different in
/// the process's own accounting, and the point of this test is the spin: the report says which one
/// it was. Both platforms are asked, because a number that is only available on one of them is
/// missing exactly where somebody is reading the failure.
fn cpu_seconds(pid: u32) -> f64 {
    if let Some(seconds) = cpu_seconds_from_proc(pid) {
        return seconds;
    }
    // No `/proc`, which means this is not Linux. `ps` is the portable answer and is only ever
    // spawned on the path where there is no `/proc` to read.
    ps_cpu_seconds(pid).unwrap_or(f64::NAN)
}

/// Linux: exact, and spawns nothing.
fn cpu_seconds_from_proc(pid: u32) -> Option<f64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The comm field is parenthesised and may contain spaces, so fields are counted from the closing
    // bracket rather than from the front of the line.
    let (_, rest) = stat.rsplit_once(')')?;
    // `utime` and `stime` are fields 14 and 15 of the line, so 12 and 13 of what follows ") ".
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let ticks: f64 = [fields.get(11), fields.get(12)]
        .into_iter()
        .flatten()
        .filter_map(|f| f.parse::<f64>().ok())
        .sum();
    // `getconf CLK_TCK` is 100 on every Linux this runs on, and this number is only ever read out of
    // a failure message.
    Some(ticks / 100.0)
}

/// Everywhere else: ask `ps`.
fn ps_cpu_seconds(pid: u32) -> Option<f64> {
    let out = std::process::Command::new("ps")
        .args(["-o", "time=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    parse_ps_time(&String::from_utf8_lossy(&out.stdout))
}

/// `[[dd-]hh:]mm:ss[.ss]`, which is what both `ps`es print -- macOS leaves `0:00.51`, Linux
/// `00:00:00`, so the fields are counted from the right rather than by position.
fn parse_ps_time(text: &str) -> Option<f64> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let (days, rest) = match text.split_once('-') {
        Some((days, rest)) => (days.parse::<f64>().ok()?, rest),
        None => (0.0, text),
    };
    let fields: Vec<&str> = rest.split(':').collect();
    if fields.is_empty() || fields.len() > 3 {
        return None;
    }
    let mut seconds = 0.0;
    for field in &fields {
        seconds = seconds * 60.0 + field.parse::<f64>().ok()?;
    }
    Some(days * 86_400.0 + seconds)
}

/// The two shapes `ps` prints, and nothing else.
///
/// Checked rather than assumed, because a parser that quietly returns `None` puts `NaN` back in the
/// failure message this exists to fill in -- and nothing would say so.
#[test]
fn the_ps_time_parser_reads_both_platforms() {
    assert_eq!(parse_ps_time("  0:00.51"), Some(0.51));
    assert_eq!(parse_ps_time("00:00:01"), Some(1.0));
    assert_eq!(parse_ps_time("0:05"), Some(5.0));
    assert_eq!(parse_ps_time("1:00:00"), Some(3600.0));
    assert_eq!(parse_ps_time("2-01:00:00"), Some(2.0 * 86_400.0 + 3600.0));
    // A process that is gone, and anything else that is not a time.
    assert_eq!(parse_ps_time(""), None);
    assert_eq!(parse_ps_time("   "), None);
    assert_eq!(parse_ps_time("no such process"), None);
    assert_eq!(parse_ps_time("1:2:3:4"), None);
}

/// This platform can answer the question the failure message asks.
///
/// Which is the whole point of asking two ways: on the machine this test is written on, the
/// `/proc`-only version returned `NaN`, so the one number that tells a spin from a hang was missing
/// exactly where somebody would be reading it.
#[test]
fn this_process_has_a_cpu_time_on_this_platform() {
    let seconds = cpu_seconds(std::process::id());
    assert!(
        seconds.is_finite() && seconds >= 0.0,
        "no CPU time for this process on this platform: {seconds}"
    );
}

/// A terminal that goes away ends the run, and does not burn a core doing it.
///
/// The child is given a pty as its standard input, output and error, so `isatty` is true and flint
/// takes its real interactive path -- the one with the key thread in it, which no other test can
/// reach (the capture hook reads lines from a pipe instead). Then the master is closed, which is the
/// hangup: the child's descriptor reports `POLLHUP` with no `POLLIN`, and nothing in crossterm
/// consumes it.
///
/// The banner is asserted *before* the hangup on purpose. A child that died at startup would
/// otherwise satisfy "it is not running any more" for the wrong reason, and a test that passes when
/// nothing works is worse than no test.
#[test]
fn a_terminal_that_goes_away_ends_the_run() {
    let home = scratch("tty-hangup");

    let mut master: libc::c_int = -1;
    let mut slave: libc::c_int = -1;
    // Both pointer arguments are spelled mutably, and that is the portable spelling rather than a
    // preference: BSD's `openpty` declares them `*mut` because it writes through them, while glibc
    // declares them `*const`. Rust coerces `*mut` to `*const` at a call and not the other way round,
    // so the mutable form is the one both accept -- and the immutable one is what this test was
    // written with, which is why it compiled on the Linux runner and nowhere else. Nothing here
    // writes to the size; `&mut` is only how glibc and BSD can be offered the same value.
    let mut size = libc::winsize { ws_row: 24, ws_col: 80, ws_xpixel: 0, ws_ypixel: 0 };
    // `&mut size` rather than `&size`, and allowed rather than worked around, because the two libcs
    // disagree about the *type* and not about the meaning: BSD declares both pointer arguments `*mut`
    // because it writes through them, glibc declares both `*const`. A `*mut` coerces to a `*const`
    // and not the other way round, so the mutable form is the one both accept -- and clippy then
    // points out, on the glibc side only, that a mutable borrow was not needed. Casting the pointer
    // to silence it would be giving up the type that makes the call correct on the other platform.
    #[allow(clippy::unnecessary_mut_passed)]
    let opened = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        )
    };
    assert_eq!(opened, 0, "openpty failed: {}", std::io::Error::last_os_error());

    // The master must not survive into the child: a child holding the master open would keep the pty
    // alive and there would be no hangup to test.
    unsafe { libc::fcntl(master, libc::F_SETFD, libc::FD_CLOEXEC) };

    let stdin = unsafe { std::fs::File::from_raw_fd(slave) };
    let stdout = stdin.try_clone().expect("clone the slave for stdout");
    let stderr = stdin.try_clone().expect("clone the slave for stderr");

    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_flint"))
        .env("FLINT_HOME", &home)
        .env_remove("NO_COLOR")
        .stdin(std::process::Stdio::from(stdin))
        .stdout(std::process::Stdio::from(stdout))
        .stderr(std::process::Stdio::from(stderr))
        .spawn()
        .expect("failed to run flint");
    let pid = child.id();

    // Wait until it is waiting for a key rather than guessing at a sleep: a fixed pause makes a
    // slow runner fail here, with a message about the wrong thing.
    let mut screen = String::new();
    let drawn = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !plain(&screen).contains("flint v0.1.0") {
        screen.push_str(&drain(master));
        assert!(
            child.try_wait().expect("try_wait").is_none(),
            "the run ended before it drew anything, so this proves nothing: {:?}",
            plain(&screen)
        );
        assert!(
            std::time::Instant::now() < drawn,
            "the run never drew its banner, so the interactive path was not the one under test: \
             {:?}",
            plain(&screen)
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    // The hangup.
    unsafe { libc::close(master) };

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let exited = loop {
        match child.try_wait().expect("try_wait") {
            Some(_) => break true,
            None if std::time::Instant::now() >= deadline => break false,
            None => std::thread::sleep(std::time::Duration::from_millis(100)),
        }
    };
    let burned = cpu_seconds(pid);
    if !exited {
        let _ = child.kill();
        let _ = child.wait();
    }
    let _ = std::fs::remove_dir_all(&home);

    assert!(
        exited,
        "the run outlived its terminal: 10s after the pty was closed it was still running, having \
         burned {burned:.1}s of CPU (a spin burns about a core; a hang burns none)"
    );
}
