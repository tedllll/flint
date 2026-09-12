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
"layout looks broken on Windows", but **`UNVERIFIED`**: nobody has watched it happen.

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
`ansi_support.rs`, quoted above, touches only the VT bit. So on Windows flint very
likely prints mojibake for any non-ASCII text, and the same applies to anything the
model writes in Chinese. `UNVERIFIED`, but the mechanism is not speculative: it is the
same one that produced the corruption below.

Cheap to fix at startup, and cheap to test: run flint on the Windows machine and see
whether the banner's `—` survives.

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

1. **Settle §1**, because it decides the shape of everything else. Ten minutes with the
   LF experiment above, no flint changes required.
2. **Check §3 at runtime** — start a session and look at the banner. If the em dash is
   mangled, the code page needs setting; that is a small, well-understood fix.
3. **Then** run the suite. `cargo test` covers the layout logic without a terminal
   through `scripts/vtscreen.js`, but that model encodes *this* machine's understanding
   of a terminal, so a passing suite on Windows is not evidence that Windows is fine.
   The tests to trust are the ones that read the real byte stream:
   `tests/term_capture.rs` with `FLINT_TERM_CAPTURE=1`.
4. **Report the layout by reading the buffer, not by eye**, when something looks wrong:

   There is no `osascript` equivalent, so the option is to make the session write its
   own byte stream and read that instead of a picture:

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
   terminal -- which is exactly why the layout code can be exercised here at all.

   On macOS the equivalent (`osascript` reading Terminal's `history`) is what settled
   the last three layout bugs, after guessing had failed several times.

## What is known to be already handled

- Windows-specific code in this repository is small — eighteen `cfg(windows)` sites,
  covering the shell dialect hint, the path separator, and test paths.
- The default config is platform-aware: `shell = "cmd"` with `args = ["/C"]` on Windows,
  and the shell probe falls back through `cmd -> powershell -> pwsh -> bash -> sh`.
- `glob` and `grep` are built in rather than shelled out, precisely because `grep` does
  not exist in `cmd.exe`, `findstr` is not recursive, and `find` does not match file
  names. See the README's Tools section.
- CI **does not run on push**. `.github/workflows/release.yml` triggers only on `v*`
  tags and `workflow_dispatch`. So nothing checks Windows unless that workflow is run by
  hand or a tag is pushed.
- This session could not even type-check for Windows: `cargo check --target
  x86_64-pc-windows-msvc` fails inside `aws-lc-sys` (rustls's C backend), which needs a
  Windows C toolchain, not just `rustup target add`.
