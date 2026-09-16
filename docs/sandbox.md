# Sandbox: a development plan

What flint's permission model is today, why the industry's "read-only / workspace-write /
full-access" ladder is the wrong shape for the tasks people actually give an agent, what a
model built out of *grants* instead of *modes* would look like, and the order to build it in.

**Status: a plan. Nothing in this file is built.** It contradicts
[`ROADMAP.md`](../ROADMAP.md)'s "Not doing, and why" entry on the permission layer
(`ROADMAP.md:1503`) on purpose; adopting any stage below means editing that entry in the same
commit, because a plan that leaves the plan of record contradicting it is how a repository
starts lying to itself.

## How to read the labels

As in [`docs/windows-tooling.md`](windows-tooling.md):

| Label | Means |
|---|---|
| `VERIFIED` | read in source, with a `file:line` — this repository, or an installed package |
| `MEASURED` | observed on this machine, by the command or the bytes given |
| `DOCUMENTED` | stated by the vendor, linked |
| `UNVERIFIED` | reasoning that has not been checked |

The measurements here were taken on Windows 11 (10.0.26200, AMD64, rustc 1.98.1) with flint
0.1.0, and against installed `@deepseek-ai/dsh` 0.1.5-rc.1 and `@openai/codex` 0.154.0.

---

## 1. The problem: a ladder cannot express a narrow request

### 1.1 What flint has

One `bool`. `VERIFIED`, `src/tools.rs:3`:

> Permission model: full by default. There is exactly one switch — `readonly`

It is enforced in six places and reported in six more:

| Where | What it does |
|---|---|
| `src/tools.rs:1452` | `bash` refuses a mutating command string |
| `src/tools.rs:1707` | `pwsh` refuses any script |
| `src/tools.rs:1845` | `exec` refuses a program that is not inspection-only |
| `src/tools.rs:2110` | `write` refuses |
| `src/tools.rs:2188` | `edit` refuses |
| `src/tools.rs:2302` | `apply_patch` refuses |
| `src/tools.rs:1879,1906,1961` | `is_readonly_command`, `is_readonly_words`, `exec_is_readonly` — the vocabulary that decides what "inspection" means |

and the word is spoken by six surfaces that have to agree: the config key, `--readonly`
(`src/main.rs:413`), `/readonly` (`src/main.rs:2614`), `/config` (`src/main.rs:2777`), the
page's `state` frame (`src/main.rs:3780`), and the `ToolBox` the tools are built from
(`src/tools.rs:45`).

That agreement is not a stylistic preference, it is this repository's scar tissue. `VERIFIED`,
`ROADMAP.md:455`:

> `/readonly on` printed "no writes, no mutating commands", the `/config` line under it printed
> `readonly = false`, and `config.toml` had no `readonly` key at all … The guard flint's own
> manual calls its only permission control had never been on, and a session that believed it was
> guarded had full permissions — worse than a command that fails, because it reports a permission
> the session does not have.

**Any model in this document inherits that rule: one truth, reported exactly as enforced.** A
grant that exists in the prompt and not in the kernel is the same lie in a new place.

### 1.2 The ladder, and why it feels wrong

Both products that ship a sandbox today use the same three rungs. `VERIFIED`,
`@deepseek-ai/dsh` `dsh-sandbox-policy/lib/index.js:74`:

> `read-only` … cannot modify files in the standing mode
> `workspace-write` … may modify files under the session workspace: `<root>`
> `danger-full-access` … does not restrict file modifications by available operations

and the escalation table is a total order over those three. `VERIFIED`,
`dsh-sandbox/lib/index.js:30`:

```js
const WIDER_MODES = {
  "read-only": ["workspace-write", "danger-full-access"],
  "workspace-write": ["danger-full-access"]
};
```

This is the part that does not fit a human task, and it is not a bug in the implementation — it
is the shape of the taxonomy. **The only thing a call can ask for is a wider mode.** So:

- The narrowest grant that lets one command write to one directory *outside* the workspace is
  `danger-full-access`: every path on the machine, the whole rest of the session.
- The narrowest grant that lets one `curl` reach one host is also `danger-full-access`, because
  the ladder has no network axis at all.
- Therefore the rational user — the one who is not going to re-approve five prompts an hour —
  switches to full access and leaves it there. The sandbox's own granularity is what teaches
  people to turn it off.

The escalation choreography is careful and generous: the request must name a strictly wider
mode, must carry a non-empty `justification`, is checked before anything executes, and the
approval answer is `allowed-once` for that one call (`dsh-sandbox/lib/index.js:51,93`). The
model is even taught to do it in the same turn, in a long verbatim paragraph
(`dsh-tool-bash/lib/index.js:129`). **All of that care is spent on a lever whose only settings
are "everything" and "nothing" for the thing that was actually denied.**

### 1.3 The axes are real, and the ladder collapses them

Four questions are being asked, and only one of them is a dial:

| Axis | The question | Expressible in the ladder? |
|---|---|---|
| Read | which paths may be read | `read-only` vs the others, roughly |
| Write | which paths may be written | only as "the workspace" or "everywhere" |
| Network | which hosts may be reached | as a mode: **not at all** (DSH); as one boolean in Codex's ladder, now per-domain rules |
| Process | which programs, with which environment | not at all (flint's `exec_is_readonly` is the closest thing, and it is a guess) |

`workspace-write` answers the network question by inheriting whatever `read-only` had: in DSH's
mode vocabulary there is no network term at all (`VERIFIED`, §3.1), so a mode name cannot say
"github.com and nothing else". Codex's ladder carried a single boolean for it
(`network_access = false`, §3.3) and the June-2026 profiles replaced that boolean with per-domain
rules — which is evidence that one bit was not enough. On Linux there is a harder version of the
same fact: **Landlock cannot express network policy** at all, which is why the products that
want egress control pair the sandbox with a proxy (§3.4).

**So "I need to download something" is not a request the ladder can grant narrowly.** It is the
canonical example of the user's complaint, and it is real.

### 1.4 What "more humane" would mean, concretely

Four properties, each testable:

1. **Predictable before the call.** The model can tell what will be allowed, from the prompt,
   without trying and failing.
2. **Explainable in one sentence when denied** — naming the thing that was missing ("writing
   outside the workspace", "network access"), not a rung ("workspace-write").
3. **Widenable by exactly the amount that was missing.** "Write here", "reach this host", "for
   this call" — not "the session is now unrestricted".
4. **Forgettable.** A grant given for a task ends with the task, unless someone writes it down
   in a hand-editable file on purpose.

---

## 2. flint's current shape, and the four things that make this cheap

The plan below is only affordable because of how flint is already built. `VERIFIED`:

| | Where | Why it matters |
|---|---|---|
| One spawn seam | `run_program_streaming`, `src/tools.rs:962` | every process — `bash`, `exec`, `pwsh` — goes through one function, and its own comment says a fix here is a fix for every tool. A sandbox has exactly one place to sit. |
| A question can already be asked mid-turn | `InputReader::ask`, `src/main.rs:949`; `InputReq::Ask`, `src/main.rs:860`; the key thread sets the input row and replies on a oneshot, `src/main.rs:885` | an approval prompt is wiring, not a subsystem |
| The prompt already has a slot for a policy sentence | `workspace.prompt_note(mode)`, `src/agent.rs:142` | the "Current file policy: …" line DSH writes is the same slot |
| Writes flint makes itself go through three functions | `write`, `edit`, `apply_patch` (`src/tools.rs:2110,2188,2302`) | path containment can be enforced in-process, today, with no OS support at all |

Two consequences worth stating before the design:

- **The cheapest half of a sandbox is not a sandbox.** Deciding "this path is outside the
  workspace" and refusing, in flint's own code, is enforceable *now*, on every platform, with
  no `unsafe`. It is a policy, not a boundary — a `bash` command can still write anything — but
  it costs almost nothing and it is honest as long as that sentence is in the prompt.
- **The expensive half is one platform at a time.** §5 stages it that way.

---

## 3. What upstream does

### 3.1 DSH — the ladder plus a careful escalator

All `VERIFIED` in the installed package.

- Three modes; default `read-only` (fail-safe), `dsh-sandbox-policy/lib/index.js:102`.
- `workspace-write` means an allow-list, derived in one place: the workspace root, `/tmp`, and
  `os.tmpdir()`, canonicalised (`dsh-sandbox/lib/index.js:155`). The comment records why
  canonicalisation is not optional: `/tmp` *is* `/private/tmp` on darwin, so an as-spelled
  grant matches nothing.
- The two knobs are bundled into user-facing **presets**, which is the humane move in the whole
  design: `workspace-write` carries `approval: ask`, `danger-full-access` carries
  `approval: never` (`dsh-permission-presets/README.zh.md`). Choosing a mode chooses how much
  you will be interrupted, in one gesture.
- Escalation: strictly wider only, `justification` mandatory and non-empty, resolved through an
  approval service *before* anything executes, `allowed-once`, and every failure path has its
  own sentence — rejection, cancellation, no approval channel, no approval service
  (`dsh-sandbox/lib/index.js:93`).
- The approval layer is deliberately poorer than the sandbox. `VERIFIED`,
  `dsh-user-approval/README.zh.md:155`: the outcome vocabulary has `allowed-once` and **no**
  `allow-always`, no remembered rules, no revocation, and no grant store; the session policy is
  only `ask` or `never`, and `never` refuses deterministically *before* any answerer is
  consulted, so a later-registered listener cannot route around it
  (`dsh-user-approval/README.zh.md:78`).
- So the two dimensions exist but are not crossed. A grant can be **narrow and one-shot** (the
  escalation refusal path: the ladder has no narrow rung, so in practice this is unavailable) or
  **wide and permanent** (switching the mode, or choosing `danger-full-access` where
  `approval: never` means you are never asked again). Nothing offers **narrow and durable** —
  "write to this one directory for the rest of this task", "reach this one host from now on".
  That missing quadrant is the whole of §4.5.
- Approvals are audited: `approval/asked` and `approval/decided` are appended with the request
  identity and a closed outcome vocabulary, and an invariant checks the pair inside one
  unfinished turn (`dsh-user-approval/README.zh.md:74,86`). The model sees the policy snapshot
  and the final tool result, never the human permission UI
  (`dsh-user-approval/README.zh.md:56`).
- Fail closed, loudly: `SandboxUnavailableError` refuses to run unconfined and names the
  alternatives for each platform (`dsh-sandbox/lib/index.js:183`).
- The model is taught the retry in the tool description, in one long paragraph, including the
  case where approval prompts are disabled — where a denial is final and escalation must not be
  attempted (`dsh-tool-bash/lib/index.js:129`). That paragraph is longer than most flint tools'
  whole descriptions, and it is the load-bearing part of the feature: it is what turns an
  opaque `Access is denied` into a one-retry conversation.
- Denials are one vocabulary, so the model recognises them identically from bash and from the
  filesystem tools: `[sandbox: file access denied under <mode> mode]` plus
  `[sandbox: escalation available — retry this exact <subject> once with sandbox_permissions
  (the narrowest wider mode that suffices) + justification; the approval prompt asks the user]`
  (`dsh-sandbox/lib/index.js:64,76`).
- Windows enforcement is not a mode flag but a runner: `dsh-sandbox-windows-acl` is **2,063
  lines** of JS driving `CreateRestrictedToken` (WRITE_RESTRICTED plus capability SIDs),
  `SetEntriesInAclW` / `SetNamedSecurityInfoW`, and `CreateProcessAsUserW` through koffi. Its
  own header (`runner.js:5`) is the most useful document in either product about what this costs:

> `workspace-write`: the workspace and temp directories carry distinct capability-SID Write
> grants; other ACL-addressable writes are denied except for the documented Everyone and
> hard-link boundaries.
> …
> In both flows the runner rewrites TMP/TEMP in its OWN environment … an explicit block through
> koffi trips ERROR_INVALID_PARAMETER in CreateProcessAsUserW, verified empirically.
> …
> Failure contract: every runner-side failure … prints `windows-acl-run: <detail>` to stderr and
> exits 127 … The child is NEVER spawned unrestricted.

Read that as a bill of materials for §5's Windows stage: separate temp SIDs, revocation,
environment rewriting, a signature so the seam can tell a runner failure from a command
failure, and a list of boundaries that are documented rather than closed.

### 3.2 Codex — two axes, and the whole ladder compiled three times

`MEASURED` on `@openai/codex` 0.154.0 (Windows x64 package). The mode vocabulary and the
per-platform implementations are all inside one 284.4 MB `codex.exe` (`.text` alone is
217.73 MB), alongside two dedicated helper programs:

| File | Size | What it carries |
|---|---|---|
| `codex.exe` | 284.4 MB | `sandbox_mode` (43 hits), `read-only` (117), `workspace-write` (32), `danger-full-access` (35), `Seatbelt` (5), `landlock` (18), `seccomp` (1), `windows-sandbox` (42), `Capability` (122) |
| `codex-windows-sandbox-setup.exe` | 14.8 MB | `CapabilitySid` (2) |
| `codex-command-runner.exe` | 7.8 MB | `CreateRestrictedToken` (2), `CreateProcessAsUser` (3), `windows-sandbox` (3) |

So Codex also uses the ladder, but it separates **sandbox mode** from **approval policy** as two
independent settings — the same insight as DSH's presets, exposed as two dials instead of one.
The three platform implementations ship in every binary: a Windows install carries macOS
Seatbelt strings and Linux Landlock strings it will never execute. That is one more cost of a
ladder nobody can port. `UNVERIFIED` for the exact config key names and the extra writable
roots / network toggles; §3.3 gathers what the documentation says.

### 3.3 Codex, since 2026-06: permission profiles — the rungs replaced by rules

`DOCUMENTED`. OpenAI shipped **Permission profiles** (beta) for Codex, described by Codex
engineering lead Thibault Sottiaux and reported by gihyo.jp on 2026-06-30
([gihyo.jp](https://gihyo.jp/article/2026/06/codex-permission-profiles), pointing at
`developers.openai.com/codex/permissions`; that page returned HTTP 403 to this document's
fetcher, so the config excerpts below are quoted from the report rather than read from the
vendor page — `DOCUMENTED`, not `MEASURED`).

Before the feature, the knobs were the ladder plus two add-ons:

```toml
sandbox_mode = "workspace-write"
approval_policy = "on-request"   # ask when a call crosses the sandbox boundary

[sandbox_workspace_write]
writable_roots = ["~/.agents"]
network_access = false
```

Those two add-ons are already an admission that the ladder is incomplete — an extra writable
root, and a network toggle — but they are global to the mode, and `.env`-style exclusions or
per-host rules are not expressible at all. What replaced them:

```toml
approval_policy = "on-request"
default_permissions = "project-edit"

[permissions.project-edit.filesystem]
":minimal" = "read"          # only what running an ordinary command needs
"~/.agents" = "write"

[permissions.project-edit.filesystem.":workspace_roots"]
"." = "write"
"**/*.env" = "deny"          # deny holds inside a wider write

[permissions.project-edit.network]
enabled = true

[permissions.project-edit.network.domains]
"api.openai.com" = "allow"   # localhost must be allowed explicitly
```

The facts worth taking from it, all from the report:

- Filesystem rules map **paths to `read` / `write` / `deny`**, and the named scopes are
  `:minimal`, `:root`, `:workspace_roots`, `:tmpdir`, `:slash_tmp`, absolute paths, `~/…`.
  `:minimal` exists precisely because "the whole filesystem" and "the workspace" are both wrong
  answers for a shell: a command needs *some* of the system to run at all.
- **More specific overrides broader, and `deny` wins for the same path** — so a workspace-wide
  `write` can still refuse `**/*.env`.
- Network is its own table with `enabled` and per-host `allow` / `deny`; `*.example.com` covers
  subdomains only, `**.example.com` covers the apex too; **deny outranks allow**; ports are not
  part of a domain rule; and **localhost is protected by default**, which is a reminder that the
  local machine is also a network destination.
- Built-in profiles are `:read-only`, `:workspace`, `:danger-full-access` — the old ladder kept
  as *presets over the rule system*, which is the migration path §4.3 should copy.
- Administrators can pin the choice with `allowed_permission_profiles` in a requirements file,
  treated as a complete allowlist.
- Windows, from the same report's field notes: restricted read-only access failed with
  `Restricted read-only access requires the elevated Windows sandbox backend` until
  `[windows] sandbox = "elevated"` was set; and **the allowlist governs command resolution** —
  a tool on `PATH` whose real directory is not readable does not run, npm globals under
  `~/AppData/Roaming/npm` need `read`, and the npm cache needs `write`.

**So §4 is not a speculative design.** It is the same move, at flint's scale: replace the mode
with rules about the call. Where a spelling already exists in the wild (`:minimal`,
`:workspace_roots`, `deny` outranking a wider allow, `*` vs `**`), flint should copy it rather
than invent a synonym, because a user who has written a Codex profile once should recognise a
flint one.

### 3.4 Claude Code: two layers, and the axes said out loud

`DOCUMENTED`: [sandboxing](https://code.claude.com/docs/en/sandboxing) and
[permissions](https://code.claude.com/docs/en/permissions).

- **Two layers with different jobs.** Permission rules decide whether a tool call runs at all
  (evaluated before anything runs, from the command text and, in auto mode, a classifier's
  judgment); sandboxing is OS-level enforcement of what a shell command may touch — "it applies
  only to Bash, PowerShell, and Monitor commands and their child processes". The vendor's own
  framing: "Path and domain restrictions … from both sandbox settings and permission rules are
  merged into the final sandbox configuration."
- **Rule grammar.** `Tool` or `Tool(specifier)` — `Bash(npm run *)`, `Read(./.env)`,
  `WebFetch(domain:example.com)` — in `allow` / `ask` / `deny` lists. Order is
  **deny → ask → allow**, first match wins, and "rule specificity doesn't change the order", so
  an allow rule cannot carve an exception out of a deny. They also warn about the obvious trap:
  a rule on the primary content field (`Bash(command:rm *)`) "would be bypassable by a compound
  command", so it is ignored with a startup warning.
- **Durability is a per-tool decision**, which is §4.5's `once` / `turn` / `always` with the
  defaults already argued: a Bash command approval is saved "permanently per repository and
  command"; a `WebFetch` domain approval "permanently per repository and domain"; a
  **file-modification approval lasts "until session end"**. Saved rules land in
  `.claude/settings.local.json` at the repository root — a hand-editable file, like the one §4
  proposes.
- **Cross-workspace is a directory list, not a mode**: the working directory plus
  `additionalDirectories`, `--add-dir`, `/add-dir`, and `/cd` to move the primary directory.
  Adding a directory grants file access only, not configuration.
- **Filesystem and network are independent layers**: `sandbox.filesystem.disabled: true` keeps
  network isolation while lifting filesystem isolation; `denyRead` / `denyWrite` hold inside a
  wider allow, and a narrower `allowRead` re-opens part of a denied region. This is §1.3's claim,
  implemented by a vendor.
- **Enforcement**: macOS Seatbelt; Linux and WSL2 bubblewrap + socat, with an optional seccomp
  filter from `@anthropic-ai/sandbox-runtime` for Unix-socket blocking. **Native Windows is not
  supported** — the documented answer is WSL2. A sandboxed command gets a writable session temp
  directory and `$TMPDIR` is set for it, which is DSH's temp-SID problem solved by a different
  mechanism.
- **The escape hatch is visible and named.** A denied command comes back to the model naming the
  path or host that was denied, and the model may retry with `dangerouslyDisableSandbox`, which
  goes through the ordinary permission flow; `allowUnsandboxedCommands: false` removes the
  retry entirely ("Strict sandbox mode"). This is DSH's escalation with a worse name and a
  better placement — the parameter is on the call rather than the mode.
- **Fail open by default, unlike DSH.** If the sandbox cannot start, Claude Code warns and runs
  unsandboxed unless `sandbox.failIfUnavailable: true`. DSH refuses to run unconfined at all.
  Two products, opposite defaults, both defensible: this is a decision flint has to make and
  write down, not copy.
- **Its limitations section is the most honest page in either product**, and is the model for
  §6: the built-in proxy does not terminate TLS by default, so the allow decision is made from
  the client-supplied hostname and **domain fronting can reach hosts outside the allowlist**;
  allowing `/var/run/docker.sock` through the sandbox is a bypass; writing to `$PATH`
  directories or `.bashrc` is privilege escalation; and the summary sentence worth quoting in
  flint's own prompt: "Effective sandboxing requires both filesystem and network isolation."

### 3.5 The lessons, stated as design rules

1. **Both leading products are replacing the ladder with per-path and per-domain rules.** The
   user's complaint in §1.2 is not a preference; it is where the field moved in 2026.
2. **Cross-workspace has a standard answer**: a list of directories. Nobody solved it with a
   wider mode.
3. **Duration is a real axis**, and the sensible defaults already exist: remembering a command
   or a domain is cheap; remembering a file-write scope is not (Claude Code expires it at
   session end).
4. **`deny` must be first-class and must outrank a wider allow**, or a broad grant silently
   re-exposes a secret.
5. **Network gets its own mechanism** — a proxy with a domain allowlist. No filesystem
   mechanism provides it, and Landlock cannot (§3.4, `DOCUMENTED`, and the reason bubblewrap is
   paired with socat).
6. **A shell needs *some* system paths to run.** `:minimal` exists because both "everything" and
   "the workspace" break ordinary commands; §5's Stage 3 should budget for the discovery of
   that list on each platform, not treat it as a detail.
7. **Say what is not enforced.** Both vendors ship a limitations page for a reason; flint's
   version is the sentence in the prompt (§4.6) plus §6 of this document.

---

## 4. The design: classify the call, not the session

### 4.1 One sentence

Replace the mode with a **grant**, make the grant a *set of facts about the call* rather than a
rung on a ladder, and let exactly one function decide.

### 4.2 Vocabulary: rules about paths and hosts, not rungs

A policy is a list of rules, and each rule names *what* and *how much*:

```
filesystem:  <path or scope> = read | write | deny
network:     <domain>        = allow | deny
exec:        <program>       = allow | deny      (flint's own judgement; advisory, §4.6)
```

The scopes are Codex's published spellings, on purpose (§3.3): `:workspace_roots`, `:tmpdir`,
`:minimal`, `:root`, `~/…`, absolute paths, and `*` / `**` globs. A user who has written a Codex
profile once should recognise a flint one; inventing synonyms would make flint's own file the
harder one to read.

Precedence, borrowed for the same reason (§3.3, §3.4):

- a narrower rule overrides a wider one;
- **`deny` wins over `read` / `write` at the same path**, so a workspace-wide `write` cannot
  silently re-expose `**/*.env`;
- network `deny` outranks `allow`.

The three familiar names survive as **presets over the rules** — which is exactly how Codex kept
its ladder (`:read-only`, `:workspace`, `:danger-full-access`, §3.3), and it is what makes this
a migration rather than a rewrite:

| Old name | The rules it expands to |
|---|---|
| `readonly` | `:workspace_roots = read`, `:minimal = read`, network off, the same exec restrictions as today |
| `workspace-write` | the above plus `:workspace_roots = write`, `:tmpdir = write` |
| `danger-full-access` | `:root = write`, network enabled |

`readonly = true` in an existing `config.toml` keeps working as the first row — the same
back-compatibility rule the `verbose` key already follows (`ROADMAP.md:489`).

### 4.3 One decision function, three outcomes, one sentence

```
decide(call, standing_grants) -> Decision {
    outcome: allow | ask | deny,
    missing: Option<Grant>,      // exactly what would make it allowed
    reason:  String,             // the sentence the model and the user both see
}
```

The invariant that keeps §1.1's scar from reopening: **the same `Decision` is what the tool
obeys, what the transcript shows, and what `/config` reports.** One function, one truth. The
reason names the missing fact, not a mode:

```
denied: write C:\work\other-repo\out.txt is outside this run's write scope
        (workspace: C:\work\flint). Missing grant: write root C:\work\other-repo.
```

### 4.4 Why `ask` is now defensible

`ROADMAP.md:1445` refuses approval prompts because "an approval dialog in an emergency is
friction at the worst moment". That objection is to a prompt on the **happy path** — a system
that interrupts ordinary work. A grant-based model has a different property: **`ask` is only
ever reached where `deny` was the alternative.** Nothing that is already allowed becomes a
question. The interruption is the price of a widening the user did not pre-authorise, and the
alternative at that exact moment is not "it just works" — it is "it fails".

That is the whole argument, and it should be written into `ROADMAP.md` when this is adopted,
because it is a change of position, not a detail.

### 4.5 Widening by exactly the missing amount

The ask carries the `missing` grant, not a mode:

| Situation | Today's ladder asks for | This asks for |
|---|---|---|
| `git clone` into a sibling checkout | `danger-full-access` (session-wide) | `write: roots:[C:\work\other-repo]`, `net: hosts:[github.com]` |
| `cargo build` failing on `~/.cargo` | `danger-full-access` | `write: roots:[%USERPROFILE%\.cargo]` |
| fetching one page | `danger-full-access` | `net: hosts:[docs.rs]`, `read: any` |
| a task that needs all three | `danger-full-access` | the same three facts |

And three durations, because "for how long" is a separate question from "how much":

- `once` — this call only.
- `turn` — the rest of this turn (the default answer to a prompt: "yes, and stop asking while
  I am doing this").
- `always` — written into `config.toml` under a `[[sandbox.grants]]` table, by hand or by
  answering a prompt with "always".

`turn` is what makes the feature usable in practice: a build tool that needs the same root
twelve times asks once.

The defaults do not have to be invented — Claude Code already argues them per tool (§3.4):
remembering a **command** or a **domain** is cheap, so those approvals are saved permanently for
the repository; remembering a **file-write scope** is not, so a file-modification approval lasts
only until the session ends. flint should start from the same split rather than giving every
kind of grant the same lifetime.

### 4.6 Where each effect can actually be enforced

Honesty requires three different labels, and the prompt has to carry them:

| Effect | Enforceable by flint itself | Needs the OS |
|---|---|---|
| `write`/`read` on a path from `write`/`edit`/`apply_patch` | **yes, today** — path containment in `src/tools.rs` | — |
| `write`/`read` from a child process (`bash`, `exec`, `pwsh`) | no | Windows restricted token + ACL, Linux Landlock, macOS Seatbelt |
| `net` | **partly**: flint's own `fetch`/`search` tools can be scoped in-process | for a child process: a proxy, or Windows WFP (a separate project) |
| `exec` | only as a guess (`is_readonly_words` today) | AppContainer / seccomp policies, or not at all |

So the design has a **floor that works everywhere**: flint's own tools are scoped now, in
process, with no `unsafe`; and a **ceiling that is per-platform**, for children. The prompt line
must say which one is in force for this run, because a `write: workspace` line that only
governs flint's own `write` tool, while `bash` is unconfined, is exactly the lie from §1.1.

Suggested shape of that line, modelled on DSH's but honest about the split:

> Current write scope: workspace `C:\work\flint` (enforced for flint's own file tools; child
> processes from `bash`/`exec` are NOT confined on this host). Network: unrestricted for child
> processes. To widen: retry the exact call once with `sandbox_permissions` naming the missing
> grant; the user is asked.

### 4.7 Three worked examples

The complaints in §1 are concrete tasks, so the design has to be checked against them rather
than against a principle. Each row below is `VERIFIED` in the sense that the *ladder's* answer
is read out of the code in §1.2 and the grant answer is what §4.2–4.5 computes; the flint
behaviour today is this repository's code.

**(a) Download something and put it outside the workspace.** `git clone https://… C:\work\tmp\x`

| | What happens |
|---|---|
| flint today | runs it. `readonly` would refuse the whole command as "not inspection". |
| The ladder | `workspace-write` refuses the write (outside the workspace), and **the network part is not gated at all** — see (c). So the step that the user can already do safely by hand is blocked, and the step that reaches the internet passes through unexamined, in the same command. |
| The grant model | `net: hosts:[github.com]` + `write: roots:[C:\work\tmp]` for this turn. The denial names the missing root, and the ask carries exactly that. |

**(b) A build that needs its own cache.** `cargo build` inside the workspace.

`workspace-write`, as implemented, allows the workspace root, `/tmp` and `os.tmpdir()`
(`dsh-sandbox/lib/index.js:155`). Cargo also writes `CARGO_HOME` under the user profile. So the
mode refuses a build in the user's own repository — not because the build is dangerous, but
because "the workspace" is a boundary that a real toolchain does not respect.
`VERIFIED` as code; not measured in a live `workspace-write` session on this machine, which is
running `danger-full-access`.

The rule model does not make this go away; it makes it *sayable*: `:workspace_roots = write`,
`~/.cargo = write`, `:tmpdir = write` is a request a human can read and grant once, instead of a
reason to move to `danger-full-access` for the rest of the week. This is the single most common
way people end up with no sandbox, and it is a granularity problem, not a discipline problem.

Codex hit the same wall from the other side and answered it in its own vocabulary (§3.3):
`:minimal` — "the minimum system areas and runtime paths needed to run ordinary commands from a
shell" — plus explicit `read` for package-manager global directories and explicit `write` for the
npm cache. Their field notes record the failure mode exactly: a tool on `PATH` whose real
directory is outside the read allowlist **does not run at all**. That is the cost of a path-based
policy, and it is paid once per machine, in a file, rather than once per task, in a prompt.

**(c) The axis the mode vocabulary cannot name.** Anything that reaches the network — `curl`,
`git`, `npm`, the agent's own `fetch` tool.

DSH's mode vocabulary has no network term (`dsh-sandbox-policy/lib/index.js:74`), its
writable-root derivation is purely paths (`dsh-sandbox/lib/index.js:155`), and its Windows
runner is a token/ACL mechanism whose own header lists only file-effect boundaries
(`dsh-sandbox-windows-acl/lib/runner.js:5`). So under `workspace-write`, `curl` to anywhere
passes unexamined while a write one directory outside the workspace is refused: **the step that
leaves the machine is ungated, and the step that stays on it is not.** Codex is the
counter-example that proves the rule needs its own vocabulary rather than a mode name — one
boolean (`network_access = false`) in the ladder, per-domain `allow` / `deny` after June 2026
(§3.3). And Claude Code documents why the axis cannot be skipped: "Without network isolation, a
compromised agent could exfiltrate sensitive files like SSH keys" (§3.4).

**A sandbox that promises to contain the agent while leaving egress alone has not contained the
one action with an irreversible, off-machine effect** — and it will still refuse a build in the
user's own directory. That inversion is the "微妙" feeling, stated as precisely as the code
allows.

### 4.8 What the first version does *not* do

Parked deliberately, with the reason, so that the next reader does not re-litigate it:

- **No shell-string judgement.** `is_readonly_words` stays a refusal vocabulary for `readonly`;
  nothing new is built on top of guessing what a command line means.
- **No network enforcement for children on Windows.** Per-host egress without a proxy means
  WFP; with a proxy it means asking users to route their tools through flint, which is a
  product decision, not a sandbox feature (§3.3 for what Claude Code does here).
- **No hard-link / reparse-point closure.** DSH documents these as boundaries rather than
  closing them (`runner.js:23`); a plan that claims otherwise would be lying.
- **No TOCTOU promise.** Deciding a path and then spawning a child leaves a window; the OS
  stage narrows it, the in-process stage cannot close it.

---

## 5. The order to build it

Each stage is useful alone, each has the test that goes red first, and each can be abandoned
without stranding the previous one. Line counts are estimates in flint's style (logic plus its
tests), not promises.

### Stage 0 — the policy as data (no OS work)

Change `readonly: bool` into a list of rules with a parsed, printed, hand-editable spelling.

- `src/config.rs`: a `sandbox` table of `filesystem` rules (`<path or scope> = read | write |
  deny`), a `network` table, and an `exec` list; the three old names expand to rule sets as
  presets (§4.2). `readonly = true` maps to the old meaning; an unknown value or a malformed
  rule is **refused rather than defaulted**, as with `verbose`.
- Precedence is implemented once and tested: narrower over wider, `deny` over `write`/`read`,
  network `deny` over `allow`.
- `src/tools.rs`: `ToolBox::new` takes the rule set; the six enforcement sites ask `decide()`
  instead of `if self.readonly`.
- `src/agent.rs`: `prompt_note` states the rules in force and which of them are enforced (§4.6).
- `/config` prints them; `/sandbox` becomes the switch (the sixth surface, `OnPage::Toggles`).
- **Red first:** three tests — a `write` to a sibling directory is refused with the sentence
  naming the path; `**/*.env` stays denied under a workspace-wide `write`; `readonly = true` in
  an old file still means what it meant.
- **Size:** 500–800 lines. **Buys:** the vocabulary, and the in-process floor. **Risk:** low.

### Stage 1 — the per-call decision, and the ask

- `decide()` in one module with its `Decision` type; the transcript shows the reason a call was
  denied, and the page shows it too (the `state`/`command` frames already exist).
- `InputReader::ask` carries the prompt for the terminal REPL; a one-shot `-p` run has no human,
  so `ask` decays to `deny` with a sentence saying so, unless a `--yes` style flag pre-grants
  it. `--web` gets the same question through the composer's input path.
- **Red first:** three tests — allow is silent, deny explains, ask is answered "once" and the
  *next* identical call asks again.
- **Size:** 400–700 lines. **Buys:** the humane half. **Risk:** medium — the three front ends
  are where "who can answer" gets decided, and `docs/web-mode.md` §4 argues against a second
  policy surface.

### Stage 2 — grants with duration, and `always`

- `once` / `turn` / `always`; `always` writes `[[sandbox.grants]]` into `config.toml`.
- Session-scoped grants are session events, and `docs/session-format.md` already has the shape
  for exactly this: a `sandbox` line carrying the whole grant set, **the last one in the file
  being the set in force**, the way `switch` is the model in force and `schema` is the answer
  shape in force (`docs/session-format.md:123,134`). Two rules come with it for free — an older
  flint skips an unknown `type` in silence, and a grant set that will not parse is reported as
  damage rather than ignored. The grants go in the line rather than a path to a file, for the
  reason `schema` gives: a session pointing at somebody's disk stops being readable when that
  file changes, and the promise of the format is that the file is the truth.
- `/sandbox show` prints the standing grants and where each came from.
- **Red first:** a grant given "for this turn" is gone at the next prompt; a grant given
  "always" survives a process restart, and `/config` names the file it came from; a session file
  with a `sandbox` line from a newer build still resumes.
- **Size:** 300–600 lines. **Buys:** the feature people actually keep on. **Risk:** low-medium.

### Stage 3 — the Windows boundary

The first stage that is a real boundary for child processes, and the largest single risk. Two
warnings from the field before it starts (`DOCUMENTED`, §3.3, §3.4):

- Codex's own notes record that restricted read-only access on Windows needs an **elevated**
  sandbox backend (`[windows] sandbox = "elevated"`), and that the path allowlist governs
  command resolution, so `:minimal` has to be discovered per machine rather than guessed.
- Claude Code supports **native Windows not at all** and documents WSL2 as the answer. A Windows
  stage that cannot be tested in CI is a stage that cannot be claimed; the alternative is to
  scope flint's Windows sandbox to what it can actually verify, or to document WSL2 as the
  supported route and refuse the claim elsewhere.

Then the work:

- Restricted token (`CreateRestrictedToken`, WRITE_RESTRICTED plus capability SIDs), ACL grants
  on the writable roots, `CreateProcessAsUserW`, plus the traps DSH paid for: a distinct temp
  SID, TMP/TEMP rewritten for the child, revocation on exit, and an exit signature so a runner
  failure is distinguishable from a command failure (§3.1).
- Interacts with two things flint already has: `KillTree` (the child tree must die with the
  turn) and `run_program_streaming`'s stdio plumbing (the terminal is flint's, not the child's).
- Fail closed, with DSH's sentence as the model, or fail open with Claude Code's warning — pick
  one, in writing, because the two vendors chose opposite defaults (§3.4).
- **Red first:** a child that tries to write one directory outside its roots fails, and the same
  child writing inside succeeds — as two tests on a real machine. Windows CI currently "checks
  nothing on push" (`ROADMAP.md:1495`), so this stage needs that fixed first or it cannot be
  verified honestly.
- **Size:** 600–1,200 lines, most of it `unsafe` and empirical. **Risk:** high.

### Stage 4 — the other two platforms

Linux and macOS. Two more CI platforms before either claim means anything, and a real choice on
Linux that the two vendors made differently (`DOCUMENTED`, §3.4): **bubblewrap** (Claude Code's
route — unprivileged user namespaces, `socat` for the network relay, and on Ubuntu 24.04 an
AppArmor profile because the default policy blocks `bwrap` from creating them) versus **Landlock**
(DSH's route — no privileges needed, but no network policy, so a proxy is still required for
egress). macOS is Seatbelt via `sandbox-exec` with a generated SBPL profile.

**Size:** 700–1,400 lines. **Risk:** high, `UNVERIFIED` from this machine.

### Stage 5 — network, only if it is a proxy

Egress control that is not a proxy is not portable (Windows: WFP; Linux: Landlock cannot; macOS:
network rules in SBPL can). If a proxy is acceptable, it is its own document. If it is not,
`net: off` stays **advisory for children** and the prompt says so.

### What every stage must keep true

- One truth per fact (the §1.1 rule).
- `config.toml` stays hand-editable; no grant exists only in memory.
- No new dependency without an argument in `docs/decisions.md`; Stage 3 is `windows-sys` or
  hand-declared `extern "system"`, which is a size-neutral choice for a 3 MB binary.
- The prompt sentence says which parts are enforced and which are advisory, on this host.

---

## 6. What this plan does not claim

- It does not claim airtightness. Stages 0–2 are a **policy**, and every sentence about them has
  to say "flint's own tools" rather than "this machine".
- It does not claim the ladder is stupid. It is the right shape for "run this in a container I
  do not trust at all"; it is the wrong shape for "this person is doing a task and one step
  reaches outside the workspace".
- It does not claim the estimates are measurements. `ROADMAP.md:1494` keeps five known defects
  honest by not repeating them; this file should keep its estimates honest the same way —
  strike them or replace them with real counts as each stage lands.
- It does not claim that a boundary is a boundary. Both vendors ship a limitations page for
  their sandboxes, and Claude Code's is the template (§3.4): the proxy does not terminate TLS by
  default, so the allow decision rests on a **client-supplied hostname** and domain fronting can
  reach hosts outside the allowlist; allowing `/var/run/docker.sock` is a bypass; write access to
  a `$PATH` directory or `.bashrc` is privilege escalation. **flint's version of that list has to
  be written before any enforcement claim is made, not after** — which is the same rule as
  `docs/windows-tooling.md`'s: label what was measured, and say what was not.

---

## 7. Sources

Read for this document, in the order they matter:

| What | Where | Label |
|---|---|---|
| The ladder's three modes, the escalation table, the fail-closed error, the workspace/temp roots | installed `@deepseek-ai/dsh` 0.1.5-rc.1 — `dsh-sandbox-policy`, `dsh-sandbox`, `dsh-permission-presets`, `dsh-user-approval`, `dsh-sandbox-windows-acl` | `VERIFIED` |
| Codex's Permission profiles, the pre-profile `sandbox_mode` / `sandbox_workspace_write` shape, the precedence and domain rules, the Windows field notes | [gihyo.jp, 2026-06-30](https://gihyo.jp/article/2026/06/codex-permission-profiles) (the vendor page it points at, `developers.openai.com/codex/permissions`, returned HTTP 403 here) | `DOCUMENTED` |
| `writable_roots` for `sandbox_workspace_write` before profiles existed | [openai/codex PR #2464](https://github.com/openai/codex/pull/2464) | `DOCUMENTED` |
| Claude Code's two layers, rule grammar and evaluation order, per-tool approval durability, `additionalDirectories`, the filesystem/network layers, Seatbelt/bubblewrap, the escape hatch, the fail-open default, and the limitations | [Sandboxing](https://code.claude.com/docs/en/sandboxing), [Permissions](https://code.claude.com/docs/en/permissions) | `DOCUMENTED` |
| The three platform implementations inside one binary, and the size of each shipped artifact | the `@openai/codex` 0.154.0 win32-x64 package on this machine, by PE section table and byte scans | `MEASURED` |

**Not incorporated, and therefore not claimed anywhere above:** Deno's scoped permission flags,
Flatpak/xdg-desktop-portal, the Android/macOS user-prompt models, and the usability literature on
prompt fatigue. They are the natural reading before Stage 1 fixes a default approval duration and
before Stage 2 chooses the lifetime of a remembered rule; nothing in §4 or §5 rests on them.
