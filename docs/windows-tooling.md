# Windows tooling: why a command string loses characters

Why a model driving `cmd.exe` and `powershell.exe` keeps losing quotes and backslashes,
what a flint tool could actually do about it, and the other Windows adaptations that fall
out of the same work.

The design here is **partly implemented**. Steps 1, 2 and one of the step-4 fixes are in the
tree and marked `done` below; everything Windows-specific is still unbuilt, because nothing
in this file has been measured on a Windows machine and those items are exactly the ones that
cannot be written from reasoning alone. The order at the end is the plan.

## How to read the labels

As in [`docs/windows.md`](windows.md), plus one more, because this file leans on Microsoft's
documentation and that is a different kind of knowledge from a measured machine:

| Label | Means |
|---|---|
| `VERIFIED` | read in this repository's source, with a `file:line` |
| `DOCUMENTED` | stated in Microsoft's documentation, linked |
| `UNVERIFIED` | reasoning that has not been checked on a Windows machine |

Nothing in this file has been measured on Windows. That is the first thing to fix.

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
  recorded in `docs/windows.md` §3, arriving through a different door. `UNVERIFIED` on the
  machine, harmless on 7.
- Preferred over `-EncodedCommand` (base64 UTF-16LE) because of the repository's own rule:
  state stays plain text a person can repair with `notepad`. A base64 blob is exactly the
  opaque thing the rules exclude, and when the script fails, being able to open the `.ps1`
  and read the real bytes is most of the debugging.
- The tool result should name the script path and the PowerShell version it ran, so the
  transcript shows what actually executed.
- Wrinkle: on a machine whose execution policy refuses scripts, `-File` is rejected.
  `-ExecutionPolicy Bypass` covers the process scope unless a group policy overrides it, in
  which case the script has to go in over stdin instead. `UNVERIFIED`; needs one check on
  the real machine.

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
escapes it.

### 6.2 GBK output becomes U+FFFD — `VERIFIED`

`clean_piece` decodes with `String::from_utf8_lossy` (`src/tools.rs:945`). On a CP936
machine, every byte a child writes in the console code page — `dir`, Windows' own error
messages, plenty of tools' messages — is not UTF-8, so the model is shown U+FFFD where the
error text was. It then reasons about a system whose errors it cannot read.

Prefer **making children speak UTF-8** over teaching flint to read GBK: the latter wants
`encoding_rs`, a dependency, and a code-page table to maintain; the former is `chcp 65001`
in the command and `PYTHONIOENCODING=utf-8` / `[Console]::OutputEncoding` in the child's
environment. Note that `docs/windows.md` §3 records the same root cause on flint's *own*
output side; this is the input side of it.

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

### 6.4 CRLF is invisible to `read` and `edit` — `VERIFIED`, decision needed

The runner splits output on both terminators (`src/tools.rs:921`, and
`split_progress_lines` at `:961`), so the transcript never carries a stray `\r`. But `read`
hands the file's bytes to the model as they are (`src/tools.rs:1188`, `from_utf8_lossy`,
nothing stripped), so a Windows file's `\r\n` reaches the model, and an `edit` whose
`old_string` was written with `\n` will not match.

Two options, and they are not equivalent: normalise on match, which is convenient but lets
an edit silently rewrite a file's line endings, or detect the mismatch and say so, which
costs a turn and keeps the file's own convention intact. Prefer the second — an unrequested
whole-file line-ending change is a diff nobody wanted and is hard to see in review.

### 6.5 In-place writes are already the Windows-correct choice — `VERIFIED`

`write`, `edit` and patch updates all call `tokio::fs::write` directly on the target
(`src/tools.rs:1273`, `:1367`, `:1478`); none of them writes a temporary file and renames it
over the target. That matters on Windows, where a rename cannot replace a file another
process holds open. Worth keeping, and worth a comment saying so — it reads like an
oversight otherwise, and it is the kind of "improvement" a later session would undo.

The one `fs::rename` in the tree is archiving a session (`src/session.rs:356`), which will
fail if that file is open in another flint. A retry or a clearer error, not a redesign.

### 6.6 Reserved names, trailing dots, long paths — `UNVERIFIED`

`CON`, `NUL`, `COM1`, a trailing dot or space in a filename, and paths past 260 characters:
Win32 truncates or rejects these, and some of it happens below the layer that reports
errors. `write` should refuse a name it cannot write literally rather than report success at
a different path. Needs one pass on the real machine to see which of these actually bite
through `std::fs` before writing any code.

### 6.7 PowerShell 5.1's redirection writes UTF-16LE — `DOCUMENTED`

`Get-Content x > y` and `Out-File` default to UTF-16LE on 5.1 (UTF-8 without BOM on 7). A
model that captures output with `>` leaves a file full of NUL bytes for `read` to show it.
Belongs in the `pwsh` tool description, and in the same breath as a nudge to
`Set-Content -Encoding utf8`.

### 6.8 There is no `sudo` — obvious, but it costs turns

Elevation is a separate UAC process that cannot be driven from a tool. `exec` should say so
plainly instead of letting the model try `sudo` and then `runas` and then `gsudo`.

### 6.9 Tell the model which PowerShell it is talking to — cheap, and no tool needed

`build_system_prompt` already probes the shell at run time and states it in "Local facts"
(`src/agent.rs:83-98`). Probing `$PSVersionTable.PSVersion` and
`$PSNativeCommandArgumentPassing` once at startup and putting them in the same list costs
one process spawn per session and removes the guess in 1.4 entirely — the model would know
whether it is on 5.1 or 7.3 rather than writing `\"` from habit.

---

## 7. The plan

In order, each one its own commit:

1. **Extract `run_program_streaming`,** with `BashTool` as its special case. Pure refactor;
   existing tests pass unchanged. (§5) — **done**
2. **`exec`,** arguments as an array. (§4.1) — **done**, registered on every platform
   rather than Windows only, with `stdin` for payloads that are not arguments (§4.3) and a
   `readonly` judgement made on the program and its verb rather than on a command line.
3. **`pwsh`,** script to a BOM'd `.ps1` and in through `-File`. Windows only. (§4.2) —
   **not started**, and deliberately so: §4.2's execution-policy wrinkle and §6.6 both want
   measurements from a real Windows session before any code is written.
4. **The three small fixes:** `\` normalisation in `glob`/`grep` (§6.3) — **done**; the
   process-tree kill (§6.1) and child output encoding (§6.2) — **not started**, both of them
   Windows-only behaviour that cannot be observed from a Unix machine.
5. **The facts in the system prompt:** PowerShell version and
   `$PSNativeCommandArgumentPassing`. (§6.9) — **not started**, same reason.

Then measure. §6.6 and the `-File` execution-policy wrinkle in §4.2 are the two places where
a real Windows session is required before any code is written; everything else in this file
is settled enough to implement.

### 6.10 Launching a browser from `/web` — `UNVERIFIED` on Windows

`--web` and `/web` call the browser rather than only printing the URL. The command is
`cmd /C start "" <url>` on Windows, and it is written from the documented behaviour rather than
from a measurement: `start` is a `cmd` builtin rather than an executable, and its **first
quoted argument is taken as the new window's title**, which is why the empty `""` is there —
without it a URL containing `&` is read as a command separator and the rest of the address is
run as a command. `std::process::Command` quoting into `cmd` is the exact class of problem the
rest of this file is about.

None of that has been tried on Windows. If it misbehaves the symptom is small and local — no
window, and the URL is printed either way, so the fallback is a paste — but the check is worth
making the first time somebody has a Windows session, alongside step 4. macOS (`open`) and
Linux (`xdg-open`) are both exercised on this machine, by replacing the program on `PATH` with
a script that records its argument.

Deliberately not doing: a PowerShell *parser* (a tool that rewrites the model's quoting for
it would be a large amount of code that is wrong in the cases that matter), `-EncodedCommand`
(opaque state, against the repository's rules), and a permission layer for `exec` beyond the
existing `readonly` switch.

As always: write the failing test first, watch it fail for the right reason, then fix the
code. The process-tree kill and the `\` normalisation are both cheap to test; the quoting
itself is best tested through `exec` by asserting the child received the exact argument
bytes, which is the only assertion that actually says anything about escaping.
