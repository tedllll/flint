# Windows tooling: why a command string loses characters

Why a model driving `cmd.exe` and `powershell.exe` keeps losing quotes and backslashes,
what a flint tool could actually do about it, and the other Windows adaptations that fall
out of the same work.

The design here is **implemented and measured**. Every step of §7 is in the tree, and the
parts that could only be settled on a real machine were settled on one — Windows 10.0.26200
(AMD64), rustc 1.98.1, PowerShell 5.1.26100.6584 as the only PowerShell on `PATH` — during the
session that wrote §6.6, §6.9 and §6.10. Where a claim below rests on a measurement it is
labelled `MEASURED` and the command is given, because the interesting results are the ones
Microsoft's documentation gets wrong.

## How to read the labels

As in [`docs/windows.md`](windows.md), plus two more, because this file leans on Microsoft's
documentation and that is a different kind of knowledge from a measured machine:

| Label | Means |
|---|---|
| `VERIFIED` | read in this repository's source, with a `file:line` |
| `DOCUMENTED` | stated in Microsoft's documentation, linked |
| `UNVERIFIED` | reasoning that has not been checked on a Windows machine |
| `MEASURED` | observed on the Windows machine described above, by the command given |

Nothing in this file is `UNVERIFIED` any more. The measurements are the reason several
predictions here turned out to be wrong, and the corrections are marked in place rather than
quietly applied.

---

## 1. Why the characters go missing

### 1.1 The chain

A model's tool call does not reach a program. It reaches a program through four or five
stages, and **every one of them re-reads the same bytes with different rules**:

```
the model's JSON tool call
   |  JSON unescaping                         \"   \\   \n
   v
flint's bash tool: one command string
   |  cmd /C <one string>                     quote stripping, %VAR%, ^
   v
   |  pwsh -NoProfile -Command <one string>
   |  PowerShell's tokenizer                  ` is the escape; \ is not
   v
   |  CreateProcess, then the child's CRT     \ is the escape again, in a string not an array
   v
the program                                  regex, JSON, embedded code
```

The model has to produce one string that survives all of them. Each stage's escaping is
correct for that stage and wrong for the next.

### 1.2 Windows has no argv at the process boundary

On Unix `execve` takes a real argv array: the kernel keeps the arguments apart and no
amount of quoting in an argument changes where it ends. On Windows `CreateProcess` takes
**one command line string**, and each runtime re-splits it using its own rules. The .NET
documentation says the quiet part out loud, in the middle of explaining PowerShell's
escaping:

> The backslash (`\`) character isn't recognized as an escape character by PowerShell. It's
> the escape character used by the underlying API for `ProcessStartInfo.ArgumentList`.
> — [`about_Parsing`](https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.core/about/about_parsing?view=powershell-7.5)

`DOCUMENTED`. So "argv" on Windows is a reconstruction, and any stage that gets the
convention slightly wrong loses a character *silently*. This is the root of everything
below.

### 1.3 PowerShell's escape character is the backtick

`\` is a literal backslash in PowerShell. `` ` `` is the escape. So the string a model
writes out of habit

```powershell
"a\"b"
```

parses as the string `a\` followed by an unterminated `"b"` — or, in other positions, as
two tokens where there was one. The symptom is exactly "the quotes disappeared".
`DOCUMENTED` (same page: "The backtick character (`` ` ``) can be used to escape any
special character in an expression").

Double quotes also still interpolate `$` and `` ` ``; only single quotes are literal.

### 1.4 Native argument quoting changed in PowerShell 7.3

`DOCUMENTED`, and it is a breaking change:

> The new `$PSNativeCommandArgumentPassing` preference variable controls this behavior.
> The valid values are `Legacy`, `Standard`, and `Windows`. The default behavior is
> platform specific. On Windows platforms, the default setting is `Windows` and
> non-Windows platforms default to `Standard`.

And in `Windows` mode, invocations of `cmd.exe`, `cscript.exe`, `wscript.exe` and anything
ending `.bat`, `.cmd`, `.js`, `.vbs`, `.wsf` **automatically use the legacy style**. So
`powershell.exe` and `pwsh.exe`, on the same machine, can disagree about how many quotes to
pass to the same native program — which is why one command works in one host and not the
other, with no error to explain it.

The case Microsoft chose to document is the one that bites:

```powershell
# the path "C:\Program Files (x86)\Microsoft\" — a trailing backslash before the closing quote
TestExe -echoargs '"C:\Program Files (x86)\Microsoft\"'
```

The stop-parsing token (`--%`) is the documented escape hatch, and it has its own edges:
`DOCUMENTED` that it is Windows-native-only, that `%NAME%` is still expanded and **cannot be
escaped** (so `%20` in a URL is not safe behind it), and that stream redirection passes
through verbatim rather than working.

### 1.5 cmd.exe is in the middle

With flint's Windows default (`shell = "cmd"`, `shell_args = ["/C"]`, `src/config.rs:52-60`)
the chain starts at `cmd.exe`, whose quote-stripping rules are their own thing, before
PowerShell is even reached. `DOCUMENTED` example from the same page:

```
PS> cmd /c echo "a|b"
'b' is not recognized as an internal or external command
```

Plus `%VAR%` expansion and `^` escaping, which will eat a `%20` or a `%Y` that the model
meant literally. So "cmd ate it" and "PowerShell ate it" are both possible, in the same run,
and the model cannot tell which from the output.

### 1.6 One character, two jobs

A quote has to group a token for the outer layer *and* be literal content for the inner
language — a regex, a JSON body, `node -e` code, a SQL string. No single character can do
both, so the correct answer is always a *mixture* (`'`, `"`, `\"`, `` `" ``, `""`) chosen
per layer. This is the part that is inherently hard rather than a model defect: there is no
"just be careful" that removes the problem.

### 1.7 The failures are silent, which is why the model never learns

The expensive property is not that escaping breaks — it is that **breaking it usually still
exits 0**. An argument splits into two, a path loses its last component, a regex loses an
anchor; the command runs, prints something plausible, and reports success. From the model's
side there is no error to read, and the tool result it sees is the text it wrote, not the
bytes that were executed. So the next turn does it again.

That is the argument for a tool, over a better prompt: a prompt cannot make a silent failure
visible, and it cannot stop the re-parsing that causes it.

### 1.8 The training prior

Models see far more bash than PowerShell. `\` as the escape character, `$VAR` interpolation,
`&&` chaining, `ls`/`cat`/`grep` — all of it is bash-shaped. On Windows that prior is wrong
in the specific way that produces this bug class, and the model has no way to know which
host it is on unless flint tells it.

---

## 2. What a tool can delete, and what it cannot

The useful distinction:

- **argv transport is lossless.** Rust's std joins `Command::args` into a Windows command
  line using the MSVCRT rules — including the trailing-backslash case in 1.4 — and the
  child's CRT splits it back with the same rules. The round trip is exact. flint already
  does the right thing at its own boundary, and says so:

  > Arguments are passed as argv, never concatenated into one string: that avoids a second
  > round of quoting rules on Windows. — `src/tools.rs:647-648`

- **`-Command`, `-e`, `-c` are lossy by design.** They hand a *string* to a language and say
  "now parse this as code". That second parse is the bug, and no amount of care in the outer
  layers removes it.

So a tool can remove every layer that re-parses **except** the one belonging to the language
the model is deliberately writing. Which gives the design: one tool that never lets a shell
see the arguments at all, and one that hands a script over as a file so it is parsed exactly
once.

What no tool fixes: PowerShell syntax that was simply wrong, and a model that believes `\`
is an escape character. The tool makes the correct idiom the easy one, which is the most a
tool can do here.

---

## 3. Where a tool goes in flint

Small surface, five places:

| Place | What it is |
|---|---|
| `src/tools.rs:18-24` | the `Tool` trait: `name`, `description`, `schema`, `call` |
| `src/tools.rs:45-80` | `ToolBox::new` — the only registration site; `readonly` is already in scope |
| `src/display.rs:331-359` | `summarise_args`: the one line the transcript shows. Unlisted tools fall back to "first string argument", which for a tool taking `program` + `args` names the wrong thing |
| `src/agent.rs:35-38` | the prompt rule that already tells the model to prefer built-in tools over shell equivalents |
| `tests/agent_loop.rs` | stub provider; enough to assert the tool is called and its arguments arrive intact |

`VERIFIED` for all five.

---

## 4. The three tools

### 4.1 `exec` — arguments as an array

```json
{
  "program": "git",
  "args": ["commit", "-m", "fix: a \"quoted\" message and a trailing C:\\dir\\"],
  "timeout_secs": 60
}
```

```rust
let mut cmd = tokio::process::Command::new(program);
cmd.args(args);          // no shell, no re-tokenisation, ever
```

This is the largest single win and it is the least clever: the model stops writing a command
line and starts writing a list. `git commit -m` with quotes, Chinese text and a trailing
backslash in one argument all become ordinary JSON string content.

Registering it on Windows only would be wrong — it is the right tool everywhere, and it
should replace `bash` for anything that is not actually shell syntax.

`readonly` can be judged from `program` + `args` rather than by sniffing a string, because
the arguments arrive already separated. That is strictly more reliable than
`is_readonly_command` (`src/tools.rs:1055`), which has to guess where the words are.

### 4.2 `pwsh` — the script arrives as a file

```json
{ "script": "...multi-line PowerShell...", "args": ["a b", "c\"d"] }
```

Write `script` verbatim to `<FLINT_HOME>/spill/<session>/N.ps1`, then run

```
pwsh -NoProfile -NonInteractive -File <path> [args...]
```

- The **script body never passes through a parser**, so the model writes normal PowerShell
  quoting and does not escape anything for an outer layer. That is the whole point.
- The file must be written **UTF-8 with a BOM**. Windows PowerShell 5.1 reads a `.ps1`
  without one as ANSI, which turns any non-ASCII text into mojibake — the same fault
  recorded in `docs/windows.md` §3, arriving through a different door. `MEASURED`: with the
  BOM bytes removed, `Write-Output "你好"` in a `-File` script came back as `浣犲ソ` with exit
  code **0** — the silent kind of wrong, and the reason the BOM is in `write_script` rather
  than left to a convention that would look like noise to a later reader.
- Preferred over `-EncodedCommand` (base64 UTF-16LE) because of the repository's own rule:
  state stays plain text a person can repair with `notepad`. A base64 blob is exactly the
  opaque thing the rules exclude, and when the script fails, being able to open the `.ps1`
  and read the real bytes is most of the debugging.
- A **counter-measurement** to the reasoning that motivated the file handover: `-Command` is
  *not* broken here. A command string with quotes, a newline, non-ASCII text and a `'b'`
  inside `"` all arrived at PowerShell intact, because PowerShell parses a command line the
  way the C runtime does and not the way `cmd` does. The file handover is still right, but
  for the other reasons: the script is an artifact a person can re-run and read, it is named
  in the result, and it is not capped by the ~32k command-line limit. Do not repeat the claim
  that `-Command` mangles quoting.
- The tool result names the script path and the PowerShell version it ran, so the transcript
  shows what actually executed: `5.1.26100.6584 (powershell) -- script: C:\...\script-1.ps1`.
- Wrinkle: on a machine whose execution policy refuses scripts, `-File` is rejected.
  `-ExecutionPolicy Bypass` covers the process scope unless a group policy overrides it.
  `MEASURED`: the effective policy on this machine is `Restricted`, and a `-File` run without
  the switch failed with "cannot be loaded because running scripts is disabled on this
  system" and exit 1. With `-ExecutionPolicy Bypass` in the argv it runs. The stdin fallback
  is therefore not needed, and is not implemented.

### 4.3 Payloads that do not belong on a command line

Give `exec` an optional `stdin`, and write into the tool descriptions that regexes, JSON
bodies and long text go in a file whose path is passed, never inline. In PowerShell scripts
specifically, `@'...'@` is the only honest multi-line literal — the terminator is `'@` at the
start of a line and nothing else is special.

---

## 5. The refactor the three share

Today the runner is a string runner: `run_command_streaming(config, command, ...)` resolves
the shell itself (`src/tools.rs:645-652`) and appends the command. Extract

```rust
run_program_streaming(program: &str, args: &[String], stdin: Option<&str>, ...) -> CommandOutcome
```

and let `BashTool` be the special case `(shell, shell_args, Some(command))`. That keeps the
existing timeout, progress, spill and streaming behaviour in one place, and it is where the
Windows fixes in the next section belong — the process-tree kill and the output encoding are
properties of *running a program*, not of the `bash` tool.

A pure refactor; the existing tests should pass unchanged, which is the point of doing it
first and alone.

---

## 6. Other Windows adaptations from the same work

Ordered by what costs the least for what it fixes.

### 6.1 A killed command does not kill its children — `VERIFIED`

The runner's only kill mechanism is `cmd.kill_on_drop(true)` (`src/tools.rs:674`). There is
no explicit kill call anywhere in the file, though the comment above it
(`src/tools.rs:670`) says "as well as the explicit kill below" — the timeout path returns
`Err` and the kill happens when `child` is dropped. Typing to interrupt does the same thing
one level up, by dropping the turn future (`src/agent.rs:367`).

`kill_on_drop` terminates the process tokio spawned and nothing else. On Unix that is
usually enough: `sh -c 'cargo build'` with a single simple command *becomes* `cargo`, so the
process killed is the build. **`cmd.exe` has no exec** — it stays as the parent and waits.
So on Windows, killing the runner kills `cmd.exe` and leaves `cargo`/`rustc` running,
holding `target/` and the release binary. The next build then fails with a lock error that
has nothing to do with the code, which is a bad afternoon for whoever has to diagnose it.

Fix: on Windows, kill the tree with `taskkill /PID <child id> /T /F`, using `Child::id()`.
No new dependency. The alternative — a Job Object — is stricter (nothing escapes, and the
job dies with flint) but needs `windows-sys`, which has to be argued against "dependencies
are the enemy" rather than assumed. Recommend `taskkill` first and revisit only if a tree
escapes it. **Built as recommended**: `KillTree::arm` in `src/tools.rs`.

`MEASURED`, and the plan above was too optimistic in two ways:

- `taskkill /T` walks the tree from a **live** parent. When the shell has already exited —
  which is what `start /b` does, and what a command that ends by backgrounding work does —
  `taskkill` answers "The process \"<pid>\" not found." with exit code **128**, and the
  children it would have walked survive. So the kill is a backstop for the *interrupt and
  timeout* cases, which is where it is used, and not a guarantee about anything a command
  deliberately detached.
- On **Unix** the gap is the same shape and is *not* fixed: `sh -c 'sleep 300 & wait'`
  leaves a child that `kill_on_drop` does not reach, because there is no process group in
  play and no `setsid` to put one there. Closing it means `libc` (a dev-dependency today) and
  either `killpg` after `setsid` or a scan of `/proc`. Unmeasured here — this machine is
  Windows — and left undone deliberately rather than half-done: the Windows path is where
  the damage was observed, and the Unix case needs a test before it needs code.

### 6.2 GBK output becomes U+FFFD — `MEASURED`, fixed differently than planned

`clean_piece` decodes with `String::from_utf8_lossy` (`src/tools.rs:945`). On a CP936
machine, every byte a child writes in the console code page — `dir`, Windows' own error
messages, plenty of tools' messages — is not UTF-8, so the model is shown U+FFFD where the
error text was. It then reasons about a system whose errors it cannot read.

Prefer **making children speak UTF-8** over teaching flint to read GBK: the latter wants
`encoding_rs`, a dependency, and a code-page table to maintain; the former is `chcp 65001`
in the command and `PYTHONIOENCODING=utf-8` / `[Console]::OutputEncoding` in the child's
environment. Note that `docs/windows.md` §3 records the same root cause on flint's *own*
output side; this is the input side of it.

**That recommendation did not survive the measurement**, and the reason is the interesting
part. `chcp` changes the *console* code page, which is shared state: the same terminal was
seen reporting 936 and then 65001 during one session, `chcp` run by a command therefore
changes the console of everything else attached to it, and a child that ignores the console
code page — Python is the standard example — still emits its own locale encoding. A fix that
depends on it is a fix that works until two commands overlap.

What was built instead: `util::decode_child_text` tries UTF-8 first and, when the bytes are
not UTF-8, decodes them with the **locale's ANSI code page** (`GetACP`, via
`MultiByteToWideChar`), which is what CPython uses and what `chcp` cannot move. No new
dependency (two hand-declared functions in `util.rs`), no table to maintain, and the honest
answer for bytes that are neither. The test is `code_page_output_reaches_the_model_as_text`,
which skips itself unless `ansi_code_page() == 936`, because on a UTF-8 machine it would be
asserting nothing.

### 6.3 `glob` and `grep` silently miss a backslash pattern — **done**

`walk_files` normalises every separator to `/` (`src/tools.rs:1769`) and `glob_match` is
documented as matching the `/`-separated relative path so it behaves the same on both
platforms (`src/tools.rs:1783-1789`). A model that writes `src\*.rs` — which it will, on
Windows, from the path separator flint itself states in the prompt (`src/agent.rs:70-73`) —
gets `no files matching 'src\*.rs'`.

A wrong answer rather than an error is the expensive kind here. Fixed by translating the
separator at the tool's door (`normalise_glob`), which is where the pattern's meaning is
known.

**This section used to say "normalise `\` to `/` in the `pattern` and `path` arguments",
which was wrong three ways, and the difference matters more than the fix:**

- `grep`'s `pattern` is **literal text to search for**, not a glob. Normalising it would
  make it impossible to search for a backslash — in a Windows path, in a regex, in code
  that quotes one. Only the `glob` *filter* argument is a pattern.
- A `path` argument goes to the filesystem through `std::path`, which already accepts
  either separator on Windows. On Unix a `\` in a filename is a legal character, and
  rewriting it would silently redirect the call to a different file.
- The glob dialect has no escape mechanism, so `\` never means anything but itself. That is
  what makes the translation safe: it can only turn a pattern that matches nothing into one
  that matches.

### 6.4 CRLF is invisible to `read` and `edit` — `MEASURED`, decided and done

The runner splits output on both terminators (`src/tools.rs:921`, and
`split_progress_lines` at `:961`), so the transcript never carries a stray `\r`. But `read`
hands the file's bytes to the model as they are (`src/tools.rs:1188`, `from_utf8_lossy`,
nothing stripped), so a Windows file's `\r\n` reaches the model, and an `edit` whose
`old_string` was written with `\n` will not match.

Two options, and they are not equivalent: normalise on match, which is convenient but lets
an edit silently rewrite a file's line endings, or detect the mismatch and say so, which
costs a turn and keeps the file's own convention intact. **The second, now built.** The
measurement is the message the model used to get, and it is the whole argument for the
change:

```
old_string not found in C:\...\dos.txt. Read the file first to get the exact text.
```

Nothing there says that a character it cannot see is the reason, so the next thing it does
is guess. The message now ends with "This file uses CRLF line endings: put \r\n between
lines in old_string", and the mirror case -- an `old_string` carrying `\r\n` against an LF
file -- says that instead. The test also asserts that an LF file gets no line-ending lecture,
and that the same edit with the `\r` in it succeeds and leaves the file's endings as they
were.

`read` still hands the bytes over unchanged, and that is a decision rather than an
unfinished item: the `\r` *is* in the text the model is shown, and a `read` that rewrote it
would be hiding the very thing the `edit` above needs to know. `apply_patch` does not yet
name the line endings when a hunk fails to match; that is the same one-sentence omission,
and it is recorded here rather than fixed blind.

### 6.5 In-place writes are already the Windows-correct choice — `VERIFIED`, commented

`write`, `edit` and patch updates all call `tokio::fs::write` directly on the target
(`src/tools.rs:1273`, `:1367`, `:1478`); none of them writes a temporary file and renames it
over the target. That matters on Windows, where a rename cannot replace a file another
process holds open. Kept, and the comment it wanted is now at the `write` call site: it read
like an oversight, and it is the kind of "improvement" a later session would undo.

The one `fs::rename` in the tree is archiving a session (`src/session.rs:356`), which will
fail if that file is open in another flint. A retry or a clearer error, not a redesign.

### 6.6 Reserved names, trailing dots, long paths — `MEASURED`, fixed

`CON`, `NUL`, `COM1`, a trailing dot or space in a filename, and paths past 260 characters:
Win32 truncates or rejects these, and some of it happens below the layer that reports
errors. `write` should refuse a name it cannot write literally rather than report success at
a different path. Needs one pass on the real machine to see which of these actually bite
through `std::fs` before writing any code.

The pass was made, and the documentation's list is not the list that bites. Through
`std::fs` on the machine described at the top, every one of these returned `Ok` from
`fs::write`:

| name | what actually happened |
|---|---|
| `NUL` | nothing was created; reading it back gave 0 bytes. The data went to the null device. |
| `trailing.` | `Ok`, and the file on disk was `trailing` |
| `trailing ` (space) | `Ok`, and the file on disk was `trailing` |
| `colon:stream.txt` | `Ok`, and the file on disk was an empty `colon`, with the bytes in an alternate data stream: invisible to `dir`, and a later `read` of the path that was written agrees with the empty file |
| `NUL.txt`, `con.txt` | real files, created and readable |
| `CON`, `COM1`, `AUX`, `LPT1`, `PRN` | real files here, and this is the version-dependent part |
| `q?`, `a\|b`, `a<b`, `a>b`, `star*` | refused by the OS: error 123, `InvalidFilename` |
| a 1619-character nested path | `create_dir_all` and `write` both `Ok` (long paths are enabled on this machine) |
| a single 300-character component | refused: error 123. 255 per component, reported honestly |

So the fix is exactly the silent cases and nothing else: the bare device names, a trailing
dot or space, and a `:` in the name. Refusing `NUL.txt` would be the same mistake in the
other direction — it is a real file here — and the illegal characters already produce an
error worth reading. `windows_name_problem` in `src/tools.rs` is the list, with the table
above as its comment; `write`, `edit` and `apply_patch` each call it before the read gate.

Two things about the implementation that are not obvious:

- It checks the **raw argument**, not the resolved path. `Path::join` parses `a:b.txt` as
  "file `b.txt` on drive A", so by the time there is a resolved path the colon is gone. This
  was found by a test that asserted the refusal for `a:b.txt` and failed: the check is about
  what the model wrote.
- The test asserts the *reason* for each name rather than that something failed. For
  `a:b.txt` the OS also fails — by resolving to drive A — so a test that accepted any error
  would have passed with the check removed. Verified red first: with the call deleted,
  `write` to `NUL` answers "wrote 5 bytes".

### 6.7 PowerShell 5.1's redirection writes UTF-16LE — `DOCUMENTED`, in the description

`Get-Content x > y` and `Out-File` default to UTF-16LE on 5.1 (UTF-8 without BOM on 7). A
model that captures output with `>` leaves a file full of NUL bytes for `read` to show it.
Belongs in the `pwsh` tool description, and in the same breath as a nudge to
`Set-Content -Encoding utf8`.

Which is where it now is, tied to the version fact from §6.9 rather than left as general
advice: the description says "On Windows PowerShell 5.1 (the version is stated in your
instructions) `>` and `Out-File` write UTF-16LE, which the `read` tool will show as a file of
NUL bytes: write files with `Set-Content -Encoding utf8` instead." The parenthetical matters
-- on 7 the warning would be wrong, and a description that is wrong about the machine is
worse than a silent one.

### 6.8 There is no `sudo` — obvious, but it costs turns — `VERIFIED`, in the description

Elevation is a separate UAC process that cannot be driven from a tool. `exec` should say so
plainly instead of letting the model try `sudo` and then `runas` and then `gsudo`.

It does: the `exec` description ends "There is no approval prompt and nothing here can answer
one, so a step that needs a password or an elevation prompt is the user's to run." `sudo` is
also in `is_readonly_words`, so `readonly` refuses it as the mutating command it is.

### 6.9 Tell the model which PowerShell it is talking to — `MEASURED`, done

`build_system_prompt` already probes the shell at run time and states it in "Local facts"
(`src/agent.rs:83-98`). Probing `$PSVersionTable.PSVersion` and
`$PSNativeCommandArgumentPassing` once at startup and putting them in the same list costs
one process spawn per session and removes the guess in 1.4 entirely — the model would know
whether it is on 5.1 or 7.3 rather than writing `\"` from habit.

Built as described: one `-Command` probe at first use (`tools::powershell`, a `OnceLock`),
preferring `pwsh` and falling back to `powershell`, and one line added to the prompt under
`#[cfg(windows)]`:

```
- PowerShell (use the `pwsh` tool): powershell 5.1.26100.6584 — arguments to native
  commands: not defined (before 7.3), so 7-only syntax (`??`, `?:`, `-Parallel`) will not parse
```

`MEASURED` through `flint debug prompt-input`, which is the honest way to see what the model
is given. The 5.x clause is there because the fact is only useful with its consequence: a
version number alone does not stop a model from writing `??`.

---

### 6.10 Launching a browser from `/web` — `MEASURED`, fixed

`--web` and `/web` call the browser rather than only printing the URL. The command was
`cmd /C start "" <url>` on Windows, and it was written from the documented behaviour rather
than from a measurement: `start` is a `cmd` builtin rather than an executable, and its **first
quoted argument is taken as the new window's title**, which is why the empty `""` is there —
without it a URL containing `&` is read as a command separator and the rest of the address is
run as a command. `std::process::Command` quoting into `cmd` is the exact class of problem the
rest of this file is about.

The doubt was well placed, and it was the *last* piece of the line that was wrong rather than
the empty quotes. Measured by putting the character under test in the *name* of a `.cmd` file
standing in for the browser — one quoted argument, in the position the URL sits — with `/b` so
that nothing opened a window, and with the child's stdio detached and a deadline, because a
`start` that opens a console window holds the pipe:

| target | `["/C", "start", "", target]` | `/S /C` and the line verbatim |
|---|---|---|
| `plain.cmd` | ran | ran |
| `a&b.cmd` | did not run: cmd read `&` as a new command | ran |
| `a^b.cmd` | did not run: the caret was eaten | ran |
| `a%20b.cmd` | ran | ran |
| `a b.cmd` | did not run | ran |

The reason is the same one as §6.1's: std quotes an argument only when it contains a space,
and a URL does not. So the URL arrived as bare text in the middle of a command line, and
`?a=1&b=2` was two commands. The fix is the form every other command string in flint now
uses — `/S /C`, the whole line quoted, the URL quoted inside it — and the empty quotes stay.
`browser_invocation` in `src/main.rs` is that line, with the table above as its comment, and
the test asserts the shape because running the real line opens a window.

macOS (`open`) and Linux (`xdg-open`) are both exercised on this machine, by replacing the
program on `PATH` with a script that records its argument. Windows has no equivalent trick —
`start` is a builtin, not a program — which is why the stand-in above is a *target* rather
than a program.

## 7. The plan

In order, each one its own commit. **All five are done**, on the machine described at the
top of this file:

1. **Extract `run_program_streaming`,** with `BashTool` as its special case. Pure refactor;
   existing tests pass unchanged. (§5) — **done**
2. **`exec`,** arguments as an array. (§4.1) — **done**, registered on every platform
   rather than Windows only, with `stdin` for payloads that are not arguments (§4.3) and a
   `readonly` judgement made on the program and its verb rather than on a command line.
3. **`pwsh`,** script to a BOM'd `.ps1` and in through `-File`, with
   `-ExecutionPolicy Bypass`. Windows only. (§4.2) — **done**; the BOM and the execution
   policy were both measured first, and both were needed.
4. **The three small fixes:** `\` normalisation in `glob`/`grep` (§6.3) — **done**; the
   process-tree kill (§6.1) — **done**, with the `taskkill /T` and Unix limits recorded
   there; child output encoding (§6.2) — **done**, as code-page decoding rather than `chcp`.
5. **The facts in the system prompt:** PowerShell version and
   `$PSNativeCommandArgumentPassing`. (§6.9) — **done**.

Step 3 also produced the one measurement that changed the design: the tool hands the script
over as a file for the reasons in §4.2, but *not* because `-Command` mangles quoting. It does
not.

Deliberately not doing: a PowerShell *parser* (a tool that rewrites the model's quoting for
it would be a large amount of code that is wrong in the cases that matter), `-EncodedCommand`
(opaque state, against the repository's rules), and a permission layer for `exec` beyond the
existing `readonly` switch.

Still open, and left open on purpose rather than by oversight: a Unix process-group kill for
backgrounded work (§6.1), and the line-ending sentence for `apply_patch` (§6.4). Both are
recorded where they belong, with what is missing and why the fix is not free.

As always: write the failing test first, watch it fail for the right reason, then fix the
code. The process-tree kill and the `\` normalisation are both cheap to test; the quoting
itself is best tested through `exec` by asserting the child received the exact argument
bytes, which is the only assertion that actually says anything about escaping.
