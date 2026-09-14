# Windows support

Written from a macOS session that could not run any of this. Everything here is
either **verified in this repository** (a dependency's source, a constant, a code path)
or **`UNVERIFIED`** — reasoning that has not been checked against a Windows machine.

Read the labels. The point of this file is that the next session starts from what is
known rather than re-deriving it, and does not mistake a plausible mechanism for a
measured one.

---

## Why this is hard at all

flint's layout is built on terminal *behaviour*, not on language features. `src/term.rs`
depends on all of these being exactly as a VT100 emulator behaves:

| Mechanism | Used for |
|---|---|
| `CSI 1;{n} r` (DECSTBM) | the scroll region that keeps history insertion off the input row |
| `CSI {row};1 H` | absolute row addressing for every transcript line |
| a newline on the region's bottom row scrolls the region | how the transcript advances |
| `ESC M` (Reverse Index) | pushing the old screen aside at startup |
| `CSI n K` | narrowing a row without moving the cursor |
| wrap behaviour on the last column | where a full row leaves the cursor |

These are not a standard. They are legacy hardware behaviour that each terminal
implements for itself, and `src/term.rs` is full of comments about the details — for
example:

> When a line fills the width exactly, the terminal leaves the cursor in the *next* row,
> so the next write's `CR` and erase land on a row that has not been drawn yet and wipes
> it.

On macOS and Linux those details are effectively uniform, because everything descends
from the same tradition. On Windows they are not, and the three sections below are the
places where that shows.

---

## 1. `DISABLE_NEWLINE_AUTO_RETURN` — the likeliest cause of a broken layout

**Verified in the dependencies.** `crossterm` enables exactly one console mode bit:

```rust
// crossterm-0.29.0/src/ansi_support.rs
fn enable_vt_processing() -> std::io::Result<()> {
    let mask = ENABLE_VIRTUAL_TERMINAL_PROCESSING;
    let console_mode = ConsoleMode::from(Handle::current_out_handle()?);
    let old_mode = console_mode.mode()?;
    if old_mode & mask == 0 { console_mode.set_mode(old_mode | mask)?; }
    Ok(())
}
```

`grep DISABLE_NEWLINE_AUTO_RETURN` over crossterm 0.29.0 finds **nothing**, and over
`src/` in this repository finds **nothing**.

That bit is the one that matters here. On a Unix terminal `\n` is LF: move down one row,
keep the column. `\r` is CR: return to column 1. Windows has a second meaning available:

| `DISABLE_NEWLINE_AUTO_RETURN` | what `\n` does |
|---|---|
| **clear** (the Windows default) | move down, and at the last column also wrap — i.e. it behaves like `\r\n` |
| **set** | move down only; wrapping at the last column does not add a second advance — i.e. Unix behaviour |

`insert_history` writes exactly this:

```rust
write!(out, "\x1b[{};1H\r\x1b[2K{text}\r\n", row);
//                                       ^^^^
```

It is counting on `\r\n` advancing **one** row. If `\n` already carries the CR, a row
that ends at the last column advances **two**, and every count that follows is wrong:
the commit accounting, the strip capacity, the scroll region. The visible result is
stray blank rows and transcript that drifts out of position — which is consistent with
"layout looks broken on Windows", and which read as the explanation for it for as long as
nobody had measured. **It is the prediction the measurement below refutes**; read that
before acting on this paragraph.

### What would settle it

A first, cheap experiment on the Windows machine, with no flint changes. The question
is whether a linefeed also returns the carriage, so give the terminal a line long enough
to fill the row and then a newline:

```cmd
more < C:\Windows\win.ini > long.txt
echo. >> long.txt
type long.txt
```

`type` emits the file's lines with `\n`, and the first line of that file is far longer
than a console row. Watch what happens at the wrap:

- every line appears on **one** row, and the row after it is the next line's -> `\n`
  moved down once, and column 1 came from the wrap
- a **blank row** appears wherever a line had to wrap -> `\n` moved down *and* the wrap
  moved down, which is the `\r\n`-like behaviour this section is about

The same thing is visible in any long line, so a simpler version is to resize the
window narrow and `type` a file with one long line in it.

A code page check belongs in the same pass, since it needs the same terminal:

```cmd
chcp
```

`936` (or anything that is not `65001`) means the console is not in UTF-8 mode, which is
section 3 below.

### What was measured — `MEASURED`, and the prediction is wrong

The premise, on the console flint inherits (10.0.26200, the session that finished
`docs/windows-tooling.md`). `GetConsoleMode` on `CONOUT$`, through the same handle chain a
console application gets:

```
mode = 0x0007
  ENABLE_PROCESSED_OUTPUT              (0x0001) = set
  ENABLE_WRAP_AT_EOL_OUTPUT            (0x0002) = set
  ENABLE_VIRTUAL_TERMINAL_PROCESSING   (0x0004) = set
  DISABLE_NEWLINE_AUTO_RETURN          (0x0008) = clear
```

So the bit this section is about is **clear**, which is the Windows default, and crossterm
never sets it (the `VERIFIED` grep above). A fresh console is the same: `0x0007`, the bit
clear, VT already on.

Two corrections for anyone repeating the measurement, both of which cost time here:

- **The bit is `0x0008`, not `0x0004`.** In the *output* mode word `0x0004` is
  `ENABLE_VIRTUAL_TERMINAL_PROCESSING`; `DISABLE_NEWLINE_AUTO_RETURN` is the next bit up.
  (In the *input* word `0x0004` is `ENABLE_ECHO_INPUT`, which is how the confusion starts.)
  A first version of this probe printed the wrong bit under the right name.
- `0x0007` also says something useful: **VT processing is already on** in this console
  before flint starts, because something enabled it and the mode persists on the screen
  buffer. crossterm's `ansi_support.rs` sets it, so an earlier interactive run is the likely
  cause — the mode is shared state, like the code page in §3.

**And then the effect was measured too, in a private console** — `FreeConsole`, `AllocConsole`,
`ShowWindow(SW_HIDE)` — so that no test bytes went into anybody's window. Position the cursor at
the last column of a 120-wide row, write, and read the cursor back, with the bit cleared and
then set, through both `WriteConsoleW` and `WriteConsoleA` (flint writes bytes, so the byte path
is the one that matters; both agree):

```
bit clear | width   then LF    | before=119,0  after=0,1
bit clear | width   then CRLF  | before=119,0  after=0,1
bit SET   | width   then LF    | before=119,0  after=119,1
bit SET   | width   then CRLF  | before=119,0  after=0,1
```

**There is no double advance.** With the bit clear — the default, and what flint gets — a
linefeed at the last column lands on the next row *and at column 0*: the newline already
includes the carriage return, exactly as this section says. What it does **not** do is advance
two rows, so `insert_history`'s `\r\n` is one row, and the same one row the bare `\n` would
have given. The accounting this section worried about is not broken by the default.

The interesting case turns out to be the opposite one. With the bit **set**, a bare `\n` at the
last column stays in the last column (119,1) and only wraps when the *next* character is
written — Unix behaviour — while `\r\n` still lands at (0,1). So flint's `\r\n` is correct under
both settings, and the thing that would break is code relying on `\n` alone. There is some of
that in flint's rendering, but nobody has to touch it because the default is the safe setting.

So: **the premise is real, the consequence is not**, and option 2 below is not needed for this
reason. That is worth stating plainly because the section above argued for it at length.

### The two ways to fix it

1. **Set the bit at startup.** `SetConsoleMode(handle, mode | DISABLE_NEWLINE_AUTO_RETURN)`.
   crossterm does not expose it, so this means calling the Windows API directly and
   taking a `windows-sys` dependency — which cuts against the reason flint has ten.
2. **Stop relying on `\n` to advance rows.** `insert_history` already positions every
   row absolutely; the trailing `\r\n` is only there to make the region scroll. Writing
   the next row's absolute position instead, and scrolling explicitly, removes the
   dependency on a platform-defined meaning of `\n`.

Option 2 fits this project's existing choices (fewer dependencies, behaviour stated in
code) and is worth preferring unless measurement says otherwise.

---

## 2. Two hosts, and they are not the same terminal

| Host | What it is |
|---|---|
| `conhost.exe` | the classic console; what `cmd.exe` and older PowerShell give you |
| Windows Terminal | the modern one; `wt.exe`, and the default on Windows 11 |

The important difference, and it is `UNVERIFIED` in detail: Windows Terminal renders VT
sequences itself, while conhost translates them into console API calls. Translation is
where semantics drift — which is also why the flag in §1 exists.

`WT_SESSION` is set in a Windows Terminal session, so the host is detectable:

```rust
let in_windows_terminal = std::env::var_os("WT_SESSION").is_some();
```

That matters because it decides the shape of the fix. If §1 is a conhost-only problem,
the answer may be a small amount of host detection. If it affects both, it is the
`\n`-dependency itself and should be removed outright.

### Which host this machine is, and one surprise — `MEASURED`

**It is conhost.** `WT_SESSION` is empty, `GetConsoleWindow` returns a window, and its title
is `Administrator: 管理员: Windows PowerShell`. Windows Terminal is not in play here, so §1 is
the conhost case, which is the harder of the two and the one this document is about.

The surprise is the screen buffer:

```
screen buffer = 126x29, window 126x29, cursor 0,0
```

**The buffer is exactly the window size, so there is no scrollback** — and that is not a
quirk of this one window: a *fresh* console allocated for the experiment in §1 came up
`120x30` with a `120x30` window, the same shape. So on this build the default is a buffer with
no history rows, not the `120x9001` the classic conhost documentation describes; the values
live in the console's stored defaults. It matters because `insert_history` is built around a
scroll region and Windows' scrollback: with no history rows, the arithmetic that decides how
much can be scrolled has nothing to work with, and the region behaves differently from the
machine the layout was designed on. Not a defect on its own — flint reads the size rather than
assuming one — but the next person measuring a layout problem should know that on this machine
"scrolled out of the window" means "gone", and that a capture which looks correct can coexist
with a window that cannot scroll back.

A `126x29` window is also narrower and shorter than the `100x24` the tests use, which is the
useful part: it is a real size that nobody chose.

### Degrading on purpose

There is a fallback worth considering if measurement shows conhost cannot be made
reliable: **do not use the scroll region there.** Fall back to plain append-only output
— no pinned input row, no reserved strip, transcript scrolling naturally.

It is much less pleasant, and it cannot break. Given what flint is for — the thing you
reach for when other tooling is broken — a layout that is plain but correct beats one
that is elegant and possibly wrong.

---

## 3. Encoding

Two separate problems share one root: **bytes written as UTF-8 being read as CP936.**

### At runtime: the console code page

A Windows console has an output code page; on a Chinese-locale machine that is CP936
(GBK). flint writes UTF-8 bytes. Unless the console is switched to UTF-8, those bytes
are interpreted as GBK and every non-ASCII character is mangled.

The switch is `SetConsoleOutputCP(CP_UTF8)` (65001). **crossterm does not do this** —
`ansi_support.rs`, quoted above, touches only the VT bit.

That was the reasoning. It was `UNVERIFIED` when it was written, and **the measurement below
says it is wrong**: flint's non-ASCII output arrives intact in a console at 936, because
Rust's standard library never hands the console those bytes. The paragraph is kept as written
because the failure is real for other writers — and because the next person will reason exactly
this way until they have read the numbers.

Cheap to check, and it was checked: run flint in a real console at 936 and see whether the
banner's `—` survives. It does.

**Measured, and the "switch the console" advice is not the whole story** (the same session
that produced `docs/windows-tooling.md` §6.2, on 10.0.26200 with locale code page 936):
`chcp` mutates *shared console state*, so a command that runs `chcp 65001` changes the
console everything else attached to it is writing to, and a child that keeps its own opinion —
CPython uses the locale's ANSI code page and ignores the console entirely — still emits
CP936. The same terminal was observed reporting 936 and then 65001 inside one session. For
*reading children's output*, what worked was decoding by `GetACP` rather than by the console
code page, and that half stands: it is what `util.rs::decode_child_text` does, and its test
runs on any CP936 machine. It is also the half where the console code page could not be
trusted, because a key another program can change is not a key.

### The numbers, and what flint's own bytes actually are — `MEASURED`

```
GetACP (locale ANSI)   = 936
GetOEMCP (OEM)         = 936
GetConsoleOutputCP     = 65001      <- and see the warning below
GetConsoleCP (input)   = 65001
```

The machine is a CP936 machine in every sense that outlives a console (`GetACP` and
`GetOEMCP` both 936), which is what `docs/windows-tooling.md` §6.2 keys the child decoding on.

**The console's own output code page read 65001, and that is very likely this session's
fault**: an earlier measurement in the same session ran `chcp 65001` to test whether the code
page could be steered, and the setting is shared, so it outlived the command that set it.
Which is the finding, not an aside — `GetConsoleOutputCP` is not a fact about the machine, it
is a fact about what has been run in this window since it opened. A fresh console starts at
the OEM code page, `936` here.

What flint itself writes was measured the way §4 of this document prefers — by reading the
bytes, not by eye. A debug build with `FLINT_TERM_CAPTURE_FILE` captured the real start screen
(2,773 bytes, session `--readonly`, `100x24`):

| in the capture | count |
|---|---|
| UTF-8 em dash `e2 80 94` | 13 |
| CP936-decoded em dash `e9 88 a5` | 0 |
| U+FFFD `ef bf bd` | 0 |

and `node scripts/vtscreen.js <capture> 24 100` draws it correctly, `readonly — writes and
mutating commands are refused` among the lines. So **flint's output is UTF-8 and nothing
inside flint mangles it** — the same conclusion `tests/term_capture.rs` reaches, now from a
real interactive start on Windows rather than from the test fixtures.

**And then the whole path was measured end to end**, with the real binary in a real console
rather than with the byte API. A hidden console — §1's private console, plus `CreateProcessW`
with `STARTF_USESTDHANDLES` so the child's stdout *is* that console — at output code page 936,
running `flint --readonly`, with the screen buffer read back afterwards:

```
console 120x30, output code page set to 936 before the run
code page while it runs: 936
  readonly <U+2014> writes and mutating commands are refused
non-ASCII code points in the buffer: U+2014     (one em dash, zero damaged)
```

**The em dash arrives intact at code page 936.** The same three bytes through the byte API
become U+9225 — measured in §1's private console, and that is what made this section's advice
look right. The binary and the byte API disagree at the same code page, so the difference
cannot be the console. It is the *writer*: Rust's standard library converts text written to a
console handle to UTF-16 and calls `WriteConsoleW`, so the console's code page never applies.
Each path flint could use, measured in the same 936 console:

| written as | what the console held |
|---|---|
| `write!`/`writeln!` — fmt, which the transcript and the banner use | U+2014 |
| `write_all` of a valid UTF-8 buffer | U+2014 |
| `write_all` of those three bytes alone | U+2014 |
| `print!` | U+2014 |
| `WriteConsoleA` of those three bytes — the byte API | U+9225 |
| a lone `0x94`, which is not valid UTF-8 | `?` |

The last two rows are the whole story: **invalid UTF-8 falls back to the byte path and is
mangled there; valid UTF-8 never takes that path at all.** Everything flint prints is a
`String`, so nothing flint prints can be mangled by the console's code page. One case could
have leaked bytes back to that path and was checked separately — a single 10,500-byte CJK write,
where the `LineWriter` inside `std::io::Stdout` could in principle split a character across a
capacity flush: it came through with every character intact.

**So there is nothing to fix.** `SetConsoleOutputCP(65001)` at startup would write shared
console state — the thing `docs/windows-tooling.md` §6.2 rejected for *reading*, for the same
reason — and would buy nothing, because the characters are already right. The previous version
of this section called the mojibake real and the fix measured-to-work, on the evidence of the
byte-API experiment plus this session's own PowerShell mangling UTF-8 files through CP936. Both
of those are real, and both are a *different program's* byte path. flint does not have one.

### At development time: the source tree, silently

This one already happened, on this project, and it is why `tests/cli_output.rs` has a
mojibake scan.

A UTF-8 em dash `—` (`e2 80 94`) was read through a CP936 code page by an editor or a
tool, decoded to two unrelated characters, and **written back to the file**:

```
U+2014  (em dash)   ->   U+9225 followed by '?'   (the damaged form, not reproduced
                                                    here: the scan in
                                                    tests/cli_output.rs would find it)
```

Fifteen user-visible strings shipped that way, including the line printed the moment a
session starts read-only. Nothing caught it:

- the file is **valid UTF-8** either way, so `cargo build` is happy
- the tests pass — the strings are not asserted on
- `clippy` is silent — there is nothing wrong with the code
- only the screen showed it, and only to someone reading it

Treat Windows as the environment where this will happen again. Concretely:

- keep the editor's encoding set to **UTF-8 without BOM**, not "system default"
- `git config core.autocrlf` and `.gitattributes` do not protect encoding, only line
  endings — a BOM or a GBK round trip is not something they catch
- the scan in `tests/cli_output.rs` will catch the common CP936 character set. It is a
  net, not a proof: it only knows the markers listed in it
- `scripts/term-layout-test.js` contains real Chinese prose, so that file is a
  reasonable canary — if its output starts printing garbage, the file was rewritten

---

## What to do first on the Windows machine

**Worked through on 2026-09-14** (10.0.26200, rustc 1.98.1, the session that finished
`docs/windows-tooling.md`), and it is finished: the two items that looked like they needed a
person at a terminal were settled in a private hidden console instead. The list is kept,
because it is the right order for the next machine, with what each item produced:

1. **Settle §1** — *settled, and the prediction was wrong.* `GetConsoleMode` says the console
   has mode `0x0007`: `DISABLE_NEWLINE_AUTO_RETURN` (0x0008) clear, VT already on, crossterm
   never sets the bit. And the effect was measured in a private console: with the bit clear, a
   linefeed at the last column lands at column 0 of the next row — one row, the same place
   `\r\n` lands. **No double advance**, so `insert_history` is not broken by the default and
   option 2 is not needed. §1 has the table, the two constant corrections, and the case where
   the bit *is* set.
2. **Check §3 at runtime** — *answered, and the answer is that there is nothing to fix.* The
   real binary was run in a hidden console at output code page 936 and its screen buffer read
   back: `readonly <U+2014> writes and mutating commands are refused`, with the em dash intact
   and nothing damaged. The same three bytes through the byte API in the same console become
   U+9225, so the difference is the writer and not the console: Rust's standard library writes
   text to a console as UTF-16, and the code page never applies. Measured for every write path
   flint uses, including a 10,500-byte single CJK write. §3 has the table; the proposed
   `SetConsoleOutputCP(65001)` is **not needed** and would only change shared console state.
   This replaced an earlier conclusion in this file that the mojibake was real — the byte-API
   experiment and this session's own PowerShell were the evidence for it, and neither is flint.
3. **Then run the suite** — *done.* `cargo test` is 345 passing on this machine, including
   `tests/term_capture.rs`, and `node scripts/term-layout-test.js` replays the byte capture
   this machine produced (`target/term-capture.bin`, written by that test run) and reports
   `全部通过`. The Chinese prose in the fixtures renders through the screen model, which is
   the canary §3 asks for.
4. **Report the layout by reading the buffer, not by eye** — *done, and it is what the numbers
   above come from*: a debug build with `FLINT_TERM_CAPTURE_FILE` wrote the real start screen to
   a file, and `scripts/vtscreen.js` drew it back at `100x24`. Nothing was read by eye, because
   there was no eye available — which is exactly why this mechanism exists, and the private
   console in §1 is the same idea taken one step further: a screen buffer nobody has to look at.

   The method, for next time: make the session write its own byte stream and read that
   instead of a picture — there is no `osascript` equivalent on Windows.

   ```cmd
   set FLINT_TERM_CAPTURE=1
   set FLINT_TERM_SIZE=100x30
   flint --readonly > screen.bin 2> errors.txt
   ```

   Then replay `screen.bin` through the same screen model the tests use, which is a
   plain Node script and runs on Windows:

   ```cmd
   node scripts\vtscreen.js screen.bin 30 100
   ```

   That prints what the terminal was told to draw, character by character, without
   anyone having to describe a screenshot. Note this only works in a debug build, and
   that `FLINT_TERM_CAPTURE` forces the interactive path while not being a real
   terminal -- which is exactly why the layout code can be exercised here at all. (Since
   `flint` itself is the only writer here, sending it to stdout is fine; a *test* uses
   `FLINT_TERM_CAPTURE_FILE=<path>` instead, because a redirected stdout would collect
   the test harness's own output too.)

   On macOS the equivalent (`osascript` reading Terminal's `history`) is what settled
   the last three layout bugs, after guessing had failed several times.

## What is known to be already handled

- Windows-specific code in this repository is small — thirty `cfg(windows)` sites in `src/`
  and five in `tests/`, covering the shell dialect hint, the path separator, the PowerShell
  probe and the `pwsh` tool, the child-text decoding, the process-tree kill, the reserved
  file names, and test paths.
- The default config is platform-aware: `shell = "cmd"` with `args = ["/C"]` on Windows,
  and the shell probe falls back through `cmd -> powershell -> pwsh -> bash -> sh`.
- `glob` and `grep` are built in rather than shelled out, precisely because `grep` does
  not exist in `cmd.exe`, `findstr` is not recursive, and `find` does not match file
  names. See the README's Tools section.
- CI **does not run on push**. `.github/workflows/release.yml` triggers only on `v*`
  tags and `workflow_dispatch`. So nothing checks Windows unless that workflow is run by
  hand or a tag is pushed.
- **Building and testing on Windows needs no cross-compilation at all**, which is worth
  saying because an earlier round recorded the opposite: `cargo check --target
  x86_64-pc-windows-msvc` from macOS fails inside `aws-lc-sys` (rustls's C backend), which
  wants a Windows C toolchain. On the machine itself `cargo test` runs natively — 345 tests,
  including the real-byte terminal tests — and that is where the Windows work was settled.
