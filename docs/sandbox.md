# Sandbox: a development plan

What flint's permission model is today, why the industry's "read-only / workspace-write /
full-access" ladder is the wrong shape for the tasks people actually give an agent, what a
model built out of *grants* instead of *modes* would look like, and the order to build it in.

**Status: a plan. Nothing in this file is built.** It contradicts
[`ROADMAP.md`](../ROADMAP.md)'s "Not doing, and why" entry on the permission layer
(`ROADMAP.md:1503`) on purpose; adopting any stage below means editing that entry in the same
commit, because a plan that leaves the plan of record contradicting it is how a repository
starts lying to itself.

## The plan in one page

flint has one switch: `readonly`, on or off for the whole session. Tasks do not come in two
sizes. A download needs the network **and** a writable directory outside the project; a sibling
checkout needs **one path**, not the machine. The ladder cannot express either, so the choice is
always "refuse" or "everything" — which is the complaint this plan exists to answer.

So: replace the switch with **rules about the call** — which paths may be read, written or must
be refused, which hosts may be reached — and later, a question when a call needs something the
rules do not already give. The three familiar names survive as **presets over the rules**, so
nothing that works today stops working.

| Stage | What is built | After it, you can… | Size | Risk |
|---|---|---|---|---|
| **0** | the rules, as config data | write `[sandbox.filesystem]` rules; `deny` one file inside a writable tree; keep `readonly = true` working | 500–800 | low |
| **1** | the per-call decision, and the question | be asked once for the missing thing, instead of flipping a session-wide switch | 400–700 | medium |
| **2** | durations, and `always` | answer "for this turn" or "always", with the durable answer in `config.toml` | 300–600 | low-medium |
| **3** | the Windows boundary | a `bash` child that really cannot write outside its roots | 600–1,200 | high |
| **4** | the Linux and macOS boundaries | the same on the other two platforms | 700–1,400 | high |
| **5** | egress, only as a proxy | reach one allowlisted host and nothing else | — | — |

**Stages 0 and 1 are the centre of gravity.** They are cross-platform, need no OS support, and
every humane property in §1.4 comes from them. Stages 3–5 are the expensive half; each is a
project on its own platform, and the platform decides whether it can be verified at all. Do 0,
then 1, then decide. Nothing here requires all six.

**Start with Stage 0, and its section in §5 is the specification** — the config syntax, the
decision function, every place in flint it has to reach, the five tests that must fail first and
the gate that must stay green. If you are here to build, read §5 beside §4.2 (`deny` and
specificity), §4.3 (one decision, one sentence) and §4.6 (what is enforced and what is only
promised). §1 and §3 are the argument and the prior art: they are why the plan is shaped this
way, and they read fine afterwards.

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

That is the diagnosis. The same facts, attached to what changes at each of them, are the table in
Stage 0 — one `bool` reaching five tool structs and six reporting surfaces is the actual size of
"replace it with rules", and it is why Stage 0 is a refactor plus a pure module rather than a
rewrite.

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

**The field record, which is stronger than the argument.** Both products' issue trackers are full
of people describing this exact inversion, and the vendors' own fixes are per-path rules. Quoted
in [`docs/research-permissions-prior-art.md`](research-permissions-prior-art.md), which labels
every quote by how it was read:

- The three-rung problem in eight words (opencode#18441): "allow → too permissive; ask → too
  noisy; deny → no access at all".
- The middle being too tight, measured by the vendor's own response: a user reported that `uv`
  could not write its cache and "Codex gets confused … and begins running unpredictable hacky
  commands" (codex#2444); a maintainer answered with the config key that fixes it —
  `sandbox_workspace_write.writable_roots = ["/Users/YOU/.cache"]` — within the hour, and the
  documentation PR for it merged the same day ([#2464](https://github.com/openai/codex/pull/2464)).
  **The middle ground the field needed was a list of paths.**
- Failing that, the middle mode gets abandoned: codex#30712, on Windows, where `apply_patch` did
  not work under the sandbox, so agents "fall back to shell-based file rewrites inside the
  project, which bypasses the sandbox" — and the reporter's conclusion: "In neither sandbox mode
  provided a functional workflow, I knowingly accepted the risks of Full access."
- Fatigue manufactures the wide grant: codex#22181, "Approval-fatigued users habitually pick (p) to
  silence prompts". Anthropic names the same disease and its cure in one post: "Constantly
  clicking 'approve' … can lead to 'approval fatigue'", and sandboxing "safely reduces permission
  prompts by 84%".

So the ladder does not fail because users are careless. It fails because its granularity is wrong
for the tasks, and the first thing every vendor did about it was let a path be named.

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
rules — which is evidence that one bit was not enough. Linux is the harder case, and the reason
egress control there is a userspace proxy rather than a kernel filter: **Landlock's network rules
are keyed on a port, not on an address** (§7, kernel documentation) — it can say "outbound TCP
to port 443" but not "only api.github.com", and seccomp cannot see hostnames either.

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

**Four more facts from the same research that this plan uses.**

- The two Codex generations **do not compose**: "Configure either `default_permissions` and
  `[permissions]`, or `sandbox_mode` / `sandbox_workspace_write`, but not both." §4.2 does the
  opposite on purpose — the three old names expand *into* rules — because two vocabularies side by
  side is what forces a "not both" rule, and a preset that expands is a rule set the user can then
  narrow.
- The approval policy is now `on-request` / `never` / `granular` (`untrusted` retired, `on-failure`
  deprecated), and a denial can be retried **exactly once**: `/approve` "applies to the exact
  denied action, not similar future actions". There is a circuit breaker at 3 consecutive and 10
  denials in 50 — the same shape as Claude Code's non-configurable 3-and-20 (§3.4). Two vendors
  independently putting a loop guard on *denials* rather than on tool calls is worth copying in
  Stage 1.
- `network.enabled` in a profile **does not start the proxy**: "To enforce profile domain rules,
  also set `features.network_proxy = true`." A network rule that is written and not enforced is
  §1.1's lie in its purest form, and it is the exact failure Stage 5 must have a test for.
- A maintainer, on review noise, states this document's thesis better than §1 does: "If too many
  mundane actions need review, fix the boundary first instead of teaching the reviewer to approve
  noisy escalations forever."

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
5. **Network gets its own mechanism** — a proxy with a domain allowlist. No filesystem mechanism
   provides it, and neither does the Linux network layer: Landlock's network rules are keyed on
   ports and seccomp cannot see hostnames, so per-host control means a userspace proxy (§3.4 for
   the proxy, §7 for the kernel documentation).
6. **A shell needs *some* system paths to run.** `:minimal` exists because both "everything" and
   "the workspace" break ordinary commands; §5's Stage 3 should budget for the discovery of
   that list on each platform, not treat it as a detail.
7. **Say what is not enforced.** Both vendors ship a limitations page for a reason; flint's
   version is the sentence in the prompt (§4.6) plus §6 of this document.

Rule 4 is the one the next subsection complicates: the conflict order above is not the only one
shipping, and one of the alternatives is better for the escape hatch. §3.6 also carries the
evidence behind rules 3, 5 and 7 — the duration designs, the port-only network layer, and what a
denial looks like when nobody writes one.

### 3.6 The capability models, and what they add to these rules

The full note, with the quotes and URLs, is
[`docs/sandbox-alternatives-research.md`](sandbox-alternatives-research.md). Six things in it
change or harden the plan above.

**1. Per-host network policy is not a kernel primitive anywhere.** Deno is the only surveyed
system where it is first-class (`--allow-net=example.com:443`, with `--deny-net` taking
precedence over the allow flags). Landlock reaches **ports**, not addresses (§7). Windows has one
capability SID (`internetClient`) — present or absent — or WFP at driver level. Flatpak's
`--share=network` is a boolean that additionally exposes every host service listening on an
abstract Unix socket. So a proxy is not a convenience, it is *the* mechanism — with Claude Code's
own caveat that the proxy trusts the client-supplied hostname (§3.4).

**2. A monotonic sandbox needs a broker outside it.** A Landlock domain, a Windows restricted
token and an SBPL profile cannot be widened once created — Landlock: "there is no way to remove
its security policy; only adding more restrictions is allowed"; Chromium: "Rules can only be
added before each target process is spawned, and cannot be modified while a target is running".
Every shipping system that asks the user at runtime therefore puts the decision *outside* the
confined process: Deno's `DENO_PERMISSION_BROKER_PATH`, Chromium's privileged parent proxying one
API call at a time, `xdg-desktop-portal` serving the Flatpak document portal. **That is exactly
flint's shape in §4.3** — `decide()` in the parent, the child confined by the OS — and it is the
strongest architectural argument in this document for it.

**3. The unit of grant can be an object, and that is the honest answer to "one file outside the
workspace".** Flatpak's document portal is the only surveyed mechanism whose unit is the object:
the user picks a file, the application receives an fd-backed handle under `/run/user/$UID/doc/`,
and read/write/delete/grant are per-application bits that can be revoked and marked transient or
persistent. §4.2's rules are class-based — a path prefix, a host — because that is what a config
file and a shell can express. The object-level grant is the better shape for a one-off, and it is
recorded as an open question (§4.8) rather than quietly omitted.

**4. Denial text is part of the mechanism, not a nicety.** Seatbelt's raw failure from the field
is `sysmond service not found` — nothing in it says "sandbox"; Codex ships a `--log-denials` flag
precisely because the default does not say; Claude Code builds the sentence itself, "naming the
path or host the sandbox denied". A model whose denials read like ordinary errors will retry,
argue, and finally get the sandbox turned off. This is the external argument for `Decision.reason`
and `Decision.missing` in §4.3.

**5. Prompt fatigue is measured, and the numbers are worse than intuition.** Felt et al., SOUPS
2012: 17% of participants paid attention to permissions at install time, and 3% could answer three
comprehension questions correctly. Wijesekera et al., USENIX Security 2015: habituation —
"if users are asked to make security decisions too frequently and in benign situations, they may
become habituated and approve all future requests"; at least 80% of participants would have
preferred to block at least one request, and **60% of permission requests occurred while the
phone screen was off**. The supported response is fewer *decisions*, not fewer permissions: ask
only where the alternative is denial (§4.4), name the missing thing, give the answer a duration
(§4.5), and let the durable answer land in a hand-editable file.

**6. Every fine-grained model collapses at the subprocess boundary, in writing.** Deno's own
documentation: `--allow-run` and `--allow-ffi` are "equivalent to --allow-all when deciding
whether to trust the code you are running", and one user was driven back to `--allow-all` by
exactly that ([deno#26839](https://github.com/denoland/deno/issues/26839): "I was very dismayed to
find that I had to replace all of my fine grained permissions with --allow-all"). Windows:
"there is no practical way to prevent code in the sandbox from calling a system service", and a
handle leaked before `LowerToken()` is an escape. So §4.6's split between what flint enforces and
what the OS enforces is not timidity — it is the state of the art, described by the people who
built it.

Two facts from that note also confirm costs already budgeted above. Microsoft's own
write-restricted-token explainer says "Writes are only possible by virtue of the service SID, the
logon SID, Everyone SID, or write-restricted SID" and "**Reads are unaffected**" — which is why
§4.6's table puts read confinement in a different column from write confinement. And on the same
page, the cost of enumerating what a program writes: "You have to determine all write accesses
your code will need and make sure you explicitly grant that access… Is it expensive? You bet it
is." That is why Stage 3 in §5 budgets for discovering `:minimal` per machine rather than
assuming it.

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

**The conflict order is a decision, and the shipping systems disagree** (§7,
[prior art](research-permissions-prior-art.md)), so it is worth naming the alternatives rather
than inheriting one:

| Order | Who ships it | What it buys |
|---|---|---|
| `deny` always wins | Cursor ("Deny rules take precedence over allow rules"); this plan | a broad allow can never silently re-expose a secret |
| first match wins, deny → ask → allow | Claude Code ("rule specificity doesn't change the order") | predictable, and an allow cannot carve an exception out of a deny |
| last match wins | OpenCode (with a catch-all `*` written first) | the specific rule is usually the one written last |
| numeric priority, highest wins | Gemini (`tier + priority/1000`) | a built-in allow-all can sit at a *low* priority, so any user rule outranks it — an escape hatch that is a rule rather than a mode |

flint should take "`deny` wins at the same path" for filesystem rules: it is the safe default, and
it is what the one product with a deny list does. Gemini's priority idea deserves a second look
*before* Stage 0 fixes the parser, because it is the only surveyed design in which "allow
everything" is an ordinary rule that a more specific rule can beat, instead of a mode that
discards the rules — which is exactly the escape hatch §4.4 needs to keep honest.

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

`ROADMAP.md:1503` refuses approval prompts because "an approval dialog in an emergency is
friction at the worst moment". That objection is to a prompt on the **happy path** — a system
that interrupts ordinary work. A grant-based model has a different property: **`ask` is only
ever reached where `deny` was the alternative.** Nothing that is already allowed becomes a
question. The interruption is the price of a widening the user did not pre-authorise, and the
alternative at that exact moment is not "it just works" — it is "it fails".

That is the whole argument, and it should be written into `ROADMAP.md` when this is adopted,
because it is a change of position, not a detail. What makes it more than a preference is the
measured half of §3.6: prompts are answered badly in bulk (17% attention, 3% comprehension), so
the design goal is not fewer permissions but fewer *decisions* — and "only where the alternative
is denial" is the smallest number of decisions that still grants anything narrow.

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

One platform constraint belongs here rather than in §5: **a Landlock ruleset is monotonic** —
once a thread is landlocked it can only be restricted further, never loosened (§7). A widening
therefore cannot be applied to a sandbox that is already running; it has to be resolved *before*
the child is spawned. flint's shape already fits that (one process per call, through
`run_program_streaming`), but a design that imagined widening a long-lived shell session would
not.

**What the duration vocabulary should be, taken from what shipped** (see
[prior art](research-permissions-prior-art.md)):

- Gemini names exactly three answers — "Allow once" / "Allow for this session" / "Allow for all
  future sessions" — and **the saved rule remembers the mode it was granted in**, under a
  hierarchy `plan < default < autoEdit < yolo`. So a grant given while the policy was loose does
  not silently survive into a stricter one. flint's `once` / `turn` / `always` should carry the
  same fact: a remembered grant records what it was granted under, and a policy change re-asks
  rather than inherits.
- OpenCode has the detail worth stealing: the prompt offers once / always-for-this-session, and
  **the tool supplies the pattern that "always" would cover** — "bash approvals typically
  whitelist a safe command prefix like `git status*`". That removes the worst part of a durable
  grant: neither the user nor the model has to invent its scope.
- The invariant that keeps a remembered rule honest, from Claude Code: a prompt may only offer
  "don't ask again" when it "can show you everything that rule would allow". **If flint cannot
  print the rule it is about to save, Stage 2 must not offer to save it.**
- A loop guard belongs on denials, not on tool calls: Codex breaks the cycle at 3 consecutive and
  10-in-50 denials, Claude Code at 3 and 20 (§3.3, §3.4). Without one, a model that keeps
  retrying a denied call turns a policy into a hang.

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

**And a run has to be labelled, not merely described.** Claude Code retitles the prompt for a
command it could not sandbox — "Bash command (unsandboxed)" instead of "Bash command" — so a human
can tell which commands ran outside the boundary (§3.4). flint's transcript should carry the same
mark on a tool line that ran unconfined, because the alternative is a transcript that reads as if
everything were contained. It is cheap, it belongs to `src/display.rs`, and it is the difference
between a policy and a claim.

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
- **No object-level grant.** Flatpak's document portal — the user picks one file, the application
  receives an fd-backed handle it can be given read/write/delete bits on, revocable, transient or
  persistent (§3.6) — is the better shape for "this one file outside the workspace", and it is the
  only surveyed mechanism whose unit of grant is the object rather than a class. It is not a fifth
  stage of this plan: it needs a broker and a handle-passing path through `bash`, which is a
  second design. What this plan does instead is make the class-based grant narrow enough
  (`C:\work\other-repo` rather than the whole machine) that the difference is survivable.

---

## 5. The order to build it

Each stage is useful alone, each has the test that goes red first, and each can be abandoned
without stranding the previous one. Line counts are estimates in flint's style (logic plus its
tests), not promises.

Every stage is laid out the same way, so a reader can find the same four things in the same
order: **After this stage** (what changes for the person using flint), **the work** (where it
goes, by file), **Done when** (the tests that must fail first and then pass), and **Not in this
stage** (what it deliberately leaves out, so the next reader does not assume it is missing).

### Stage 0 — the policy as data (no OS work)

**After this stage:** a `config.toml` can name the paths that are writable and the paths that
are refused outright, `deny` beats a wider `write`, and every surface that reports the policy
reports the same thing. Nothing about the OS, the terminal or the model changes; `readonly` in an
old config still does exactly what it did.

**The rule language — the whole user-visible surface of this stage:**

```toml
[sandbox]
mode = "workspace"                     # optional preset: read-only | workspace | danger-full-access

[sandbox.filesystem]
":workspace_roots" = "write"           # the working directory
":minimal" = "read"                    # the system paths a shell needs to run at all (§3.5 rule 6)
":tmpdir" = "write"                    # the temp directory; rewritten for children in Stage 3
"**/*.env" = "deny"                    # wins over any wider write — §4.2
"C:\\work\\other-repo" = "write"        # the sibling checkout, named exactly

[sandbox.network]
"crates.io" = "allow"
"*" = "deny"
```

Access words are `read`, `write`, `deny`; the scope tokens are `:workspace_roots`, `:minimal`,
`:tmpdir`, `:slash_tmp`, `:root`; globs are `*` (one component) and `**` (any depth). The
spellings are Codex's on purpose (§3.6, rule 1): somebody who has written one Codex profile
should recognise a flint one. **An unknown access word, an unknown token or a malformed glob is
refused at load, naming the key** — not defaulted, following the `verbose` precedent
(`ROADMAP.md:489`).

**The decision, in code.** One function, and everything else asks it:

```rust
// src/sandbox.rs — pure, no I/O, no OS calls
pub struct Policy { filesystem: Vec<FsRule>, network: Vec<NetRule> }

pub enum Outcome { Allow, Deny }        // Ask arrives in Stage 1; Stage 0 never returns it
pub struct Decision { pub outcome: Outcome, pub missing: Option<String>, pub reason: String }

impl Policy {
    pub fn decide_read(&self, path: &Path) -> Decision;
    pub fn decide_write(&self, path: &Path) -> Decision;
    pub fn decide_network(&self, host: &str) -> Decision;
    pub fn is_read_only(&self) -> bool;  // the question `/readonly` asks today
}
```

`missing` and `reason` are built in Stage 0 even though nothing asks yet, because §1.1's scar is
a surface that reported a rule nothing enforced: the sentence is written **where the decision is
made**, not where it is printed, so no front end can invent a different one.

**The work, place by place.** This is the part that is easy to underestimate: `readonly` is a
`bool` copied into five tool structs, and it is reported on six surfaces.

| Place | Today | Stage 0 |
|---|---|---|
| `src/sandbox.rs` | does not exist | new module: rules, parsing, specificity, `deny` precedence, `decide()` |
| `src/config.rs:183` | `pub readonly: bool` | a `sandbox` key beside it; `readonly = true` resolves to the read-only preset |
| `src/agent.rs:155,216,227` | `readonly: bool`, `ToolBox::new(config, readonly, cwd)` | carries a `Policy`; `Agent::readonly()` (`:331`) stays a derived getter so the REPL is untouched |
| `src/tools.rs:45,60,65,73,78,83,101` | `ToolBox::new` copies the bool into five tools | one `Policy`, shared |
| `src/tools.rs:505,1590,1773,2081,2157,2266` | five tool structs hold `readonly: bool` | hold the `Policy` |
| `src/tools.rs:2110,2188,2302` | `write`/`edit`/`patch` refuse when `readonly` | call `decide_write(path)`; the refusal **names the path and the rule that refused it** |
| `src/tools.rs:1452,1707,1845` | `bash`/`pwsh`/`exec` refuse via `is_readonly_command` | unchanged when the policy is read-only — a command *string* has no paths to judge, so Stage 0 does not pretend it does (§4.6) |
| `src/main.rs:413–416` | `cfg.readonly \|\| args.readonly` | the same, expressed as the read-only preset, so `--readonly` keeps its "only ever turns it on" meaning |
| `src/main.rs:2614`, `/readonly` | toggles `cfg.readonly` | toggles the preset; if a hand-written rule contradicts the toggle, it is **reported, not overwritten** (the comment at `:2632` records the bug this must not bring back) |
| `src/main.rs:2777`, `/config` | prints `readonly = true/false` | prints the rules in force, and marks which are enforced and which are advisory on this host (§4.6) |
| `src/main.rs:3780`, the page's `state` frame | `{"name":"readonly","values":[…],"value":…}` | shape unchanged; the value reports the preset, so the page needs no new code |
| `src/context.rs:123`, `prompt_note` | describes `readonly` | states the rules in force, and that they bind flint's own tools and not `bash`'s children |

**Done when:** each of these is a test written first, watched fail for the right reason:

1. a `write` outside the rules is refused, and the message names the path;
2. `"**/*.env" = "deny"` stays denied under a workspace-wide `write` — `deny` wins;
3. a narrower `write` beats a wider `read` — specificity;
4. `readonly = true` in an old config refuses exactly what it refused before, and `/config`
   prints it as the preset it now is;
5. a malformed rule is refused at load, naming the key;
6. `cargo test`, `cargo clippy --all-targets` (silent) and `node scripts/term-layout-test.js`
   are green.

**Not in this stage:** any question, any duration, any OS boundary, any enforcement for child
processes. What Stage 0 confines is flint's own `write`, `edit` and `apply_patch`; the prompt
says so in those words.

**Size:** 500–800 lines, tests included. **Risk:** low — one bool becomes one shared `Policy`,
plus a pure module that is testable on every platform today.

### Stage 1 — the per-call decision, and the ask

**After this stage:** a call that needs something the rules do not already give is **asked about
once** — in the terminal, in `-p`, or in the page — instead of the answer being a session-wide
switch. Stage 0's refusal already names the missing grant; this stage makes that grant
answerable.

- `decide()` grows a reachable `Ask`; the transcript shows the reason a call was denied or
  asked about, and the page shows it too (the `state`/`command` frames already exist).
- The three front ends: `InputReader::ask` (`src/main.rs:949`) carries the prompt for the REPL; a
  one-shot `-p` run has no human, so `ask` decays to `deny` with a sentence saying so, unless a
  `--yes` style flag pre-grants it; `--web` gets the same question through the composer's input
  path. `Ask` is only returned where `Deny` was the alternative (§4.4), so nothing already
  allowed becomes a question.
- The same `Decision` is what the tool obeys, what the transcript shows and what `/config`
  reports (§4.3): one truth per fact, which is the rule Stage 0 set up.
- The question offers only what it can print: if the rule an `always` answer would save cannot be
  shown in the question, the question does not offer `always` (§4.5). Durations themselves are
  Stage 2; here the answer covers the current call.
- A denial circuit breaker — stop asking after N consecutive or M-in-window denials and say why
  (§4.5) — because a model that keeps retrying turns a policy into a hang.
- A tool line that ran unconfined carries the mark in the transcript (§4.6).
- A workspace-trust question is the one cheap place to enumerate a grant set *before* any work
  starts ("this folder would grant: workspace write, temp write, no network"), and it is the
  natural home for the durable answer.
- **Done when:** allow is silent; deny explains and names the missing grant; the answer is what
  the tool actually obeys; the *next* identical call asks again (nothing is remembered yet); a
  `-p` run with no human denies in one sentence instead of hanging.
- **Not in this stage:** remembering an answer.
- **Size:** 400–700 lines. **Buys:** the humane half. **Risk:** medium — the three front ends
  are where "who can answer" gets decided, and `docs/web-mode.md` §4 argues against a second
  policy surface.

### Stage 2 — grants with duration, and `always`

**After this stage:** an answer can be remembered — for this turn, or for good — and the durable
one is a line in `config.toml` that can be read and edited by hand. This is the feature people
actually keep switched on, because it is the one that stops the asking (§3.6, rule 5).

- `once` / `turn` / `always`; `always` writes `[[sandbox.grants]]` into `config.toml`, and the
  saved rule records **what it was granted under**, so a later, stricter policy re-asks rather
  than inherits the old answer (§4.5).
- Session-scoped grants are session events, and `docs/session-format.md` already has the shape
  for exactly this: a `sandbox` line carrying the whole grant set, **the last one in the file
  being the set in force**, the way `switch` is the model in force and `schema` is the answer
  shape in force (`docs/session-format.md:123,134`). Two rules come with it for free — an older
  flint skips an unknown `type` in silence, and a grant set that will not parse is reported as
  damage rather than ignored. The grants go in the line rather than a path to a file, for the
  reason `schema` gives: a session pointing at somebody's disk stops being readable when that
  file changes, and the promise of the format is that the file is the truth.
- The tool supplies the pattern that `always` would cover, so neither the user nor the model has
  to invent its scope (§4.5).
- `/sandbox show` prints the standing grants and where each came from.
- **Done when:** a grant given "for this turn" is gone at the next prompt; a grant given
  "always" survives a process restart, and `/config` names the file it came from; a session file
  with a `sandbox` line from a newer build still resumes.
- **Not in this stage:** any OS boundary, and any grant that a rule in `config.toml` cannot
  express.
- **Size:** 300–600 lines. **Risk:** low-medium.

### Stage 3 — the Windows boundary

**After this stage:** on Windows, a child process flint starts cannot write outside the roots the
policy names, and that boundary is the operating system's rather than flint's word. It is the
first stage where §4.6's sentence changes from "advisory" to "enforced" for children.

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
- **Done when:** a child that tries to write one directory outside its roots fails, and the same
  child writing inside succeeds — as two tests on a real machine. This used to end "Windows CI currently
  checks nothing on push, so this stage needs that fixed first or it cannot be verified honestly", and
  that stopped being true on 2026-09-18: `.github/workflows/ci.yml` runs `cargo test` and `cargo clippy`
  on `windows-latest` and `ubuntu-latest` on every push, plus the two headless Node harnesses. What the
  stage still needs is for the boundary itself to be testable *there* — a question about the boundary
  rather than about CI.
- **Not in this stage:** Linux, macOS, and egress control. A boundary that exists on one platform
  and is labelled as one-platform in the prompt is still worth having, and it is the only way to
  find out what the other two will cost.
- **Size:** 600–1,200 lines, most of it `unsafe` and empirical. **Risk:** high.

### Stage 4 — the other two platforms

**After this stage:** the same boundary on the other two platforms, each with the mechanism that
platform actually provides — and a written answer for the cases where it provides none.

Linux and macOS. Two more CI platforms before either claim means anything, and a real choice on
Linux that the two vendors made differently (`DOCUMENTED`, §3.4): **bubblewrap** (Claude Code's
route — unprivileged user namespaces, `socat` for the network relay, and on Ubuntu 24.04 an
AppArmor profile because the default policy blocks `bwrap` from creating them) versus
**Landlock** (DSH's route — no privileges needed, network rules since ABI v4 for TCP and v10 for
UDP but keyed on **ports**, so per-host egress still means a proxy). macOS is Seatbelt via
`sandbox-exec` with a generated SBPL profile.

**Landlock is monotonic, and that is a design constraint, not a footnote**: a landlocked thread
can only be restricted further, never loosened (§7). So a widening can never be applied to a
running sandbox — it has to be resolved *before* the child is spawned. flint already has that
shape (one process per call, through `run_program_streaming`), which is why §4.5's `once`/`turn`
durations are implementable at all.

**Done when:** a child refused outside its roots on each platform, in that platform's CI, and a
written answer for every case where the platform offers no mechanism — including the one this
survey expects on macOS, where the interface is deprecated and undocumented (§3.6, §7).

**Size:** 700–1,400 lines. **Risk:** high, `UNVERIFIED` from this machine. **Not in this stage:**
egress — this stage confines the filesystem, and the network stays what Stage 3 made it.

### Stage 5 — network, only if it is a proxy

**After this stage:** a child can reach one allowlisted host and nothing else, and the allowlist
is the same list the policy already prints.

**Done when:** a child that reaches an allowlisted host succeeds and one that reaches any other
host fails, both as tests against the proxy — and the prompt's sentence changes from "advisory"
to "enforced" for the network, which is the only thing that makes this stage worth building.

**Not in this stage, and not in any stage:** a claim that flint filters by address without a
proxy. Egress control that is not a proxy is not portable (Windows: WFP; Linux: Landlock reaches
ports, not hosts; macOS: SBPL can name hosts, but `sandbox-exec` is deprecated). If a proxy is
acceptable, it is its own document. If it is not, `net: off` stays **advisory for children** and
the prompt says so — which is the honest end state, not a failure.

**Size:** deliberately unestimated. This stage is not a line count but a decision — whether flint
is willing to run a second process and route children through it — and the estimate is only
honest after that decision, in a document of its own.

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
| Landlock's network rules are keyed on a **port** (ABI v4 for TCP, v10 for UDP), and a ruleset is monotonic — "there is no way to remove its security policy; only adding more restrictions is allowed" | [Landlock userspace API](https://docs.kernel.org/userspace-api/landlock.html), [Landlock security](https://docs.kernel.org/security/landlock.html) | `DOCUMENTED` |

**Corrections made in place.** An earlier version of this file said Landlock "cannot express
network policy" (§1.3, §3.5, Stage 4, Stage 5). That was wrong twice: network rules do exist, and
a ruleset cannot be loosened. The kernel documentation is now the source for both claims, and
both are stated as what it says rather than as what this document first assumed. Recorded here
rather than quietly edited, for the reason [`docs/windows-tooling.md`](windows-tooling.md) gives
about its own wrong predictions.

Two research notes hold the material this document summarises, and both are worth reading in full
before the stages that use them:

| What | Where |
|---|---|
| The capability and scope models: Deno's scoped flags and its permission broker, Flatpak's document portal, Landlock's ABI, Windows restricted tokens / AppContainer / WFP, Seatbelt's undocumented profiles, and the prompt-fatigue studies | [`docs/sandbox-alternatives-research.md`](sandbox-alternatives-research.md) — read before Stage 3 and Stage 4 |
| Prior art across coding-agent CLIs: Codex's two non-composable generations, Claude Code's six modes in three layers, Gemini's policy engine, Cursor's rule tokens, OpenCode's session grants, Amp, Aider — and the issue-tracker record of the ladder failing in practice (§1.2) | [`docs/research-permissions-prior-art.md`](research-permissions-prior-art.md) — read before Stage 0, which is where the conflict order gets frozen |

**Still not incorporated, and therefore not claimed anywhere above:** the Android and Apple
permission-model wording (both vendor sites returned only JavaScript shells to the fetcher),
Chromium's macOS Seatbelt design note (a pointer, not a read), SBPL's network syntax (no primary
source found), and "The Attacker Moves Second" (read only through a summary). No peer-reviewed
study of agent sandbox escapes was found at all, so §3.6's escape evidence is issue trackers and
vendor documentation — weaker, and labelled as what it is.
