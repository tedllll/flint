# Permissions and sandboxing in coding-agent CLIs — prior art

Research notes for the design doc. Written 2026-09-16. Everything quoted below was read off
a page loaded in this session; every claim that could not be verified from a loaded page is
marked **[UNVERIFIED]**. Where the only reachable copy of a vendor page was a mirror, that is
said in the entry — the official URL is given first, then the copy actually read.

Confidence labels, same idea as `docs/windows-tooling.md`:

| Label | Means |
|---|---|
| `LOADED` | the sentence was read off the page at the URL given |
| `MIRROR` | read off a third-party copy of a page that 403s or hides its body from this fetcher |
| `UNVERIFIED` | stated here as a question, not as a fact |

---

## 1. OpenAI Codex CLI

### (a) Knobs and values

Two generations of configuration are live at once, and they do not compose.

**Legacy: `sandbox_mode` × `approval_policy`** (`LOADED`, config reference; mirror of
`learn.chatgpt.com/docs/config-file/config-reference`):

- `sandbox_mode` — `"read-only"` | `"workspace-write"` | `"danger-full-access"`.
- `approval_policy` — `"untrusted"` (**retired**), `"on-failure"` (**deprecated**),
  `"on-request"`, `"never"`, and `"granular"`.
  - A commit titled `chore(core) Deprecate approval_policy: on-failure (#11631)` exists
    (`LOADED`, commit page); the mirrored managed-requirements docs list the supported set as
    "such as `on-request`, `never`, and `granular`" (`LOADED`).
  - The docs tell users to migrate off `untrusted` entirely: "Codex and ChatGPT Work no longer
    support `approval_policy = "untrusted"`. The retired setting can prevent either client
    from starting."
- The reference also shows `sandbox_mode = "workspace-write"` accepting four sub-keys, whose
  descriptions I read verbatim off the mirror of the reference (`MIRROR`):
  - `sandbox_workspace_write.writable_roots` — "Additional writable roots when
    `sandbox_mode = "workspace-write"`."
  - `sandbox_workspace_write.network_access` — "Allow outbound network access inside the
    workspace-write sandbox."
  - `sandbox_workspace_write.exclude_tmpdir_env_var` — "Exclude `$TMPDIR` from writable roots
    in workspace-write mode."
  - `sandbox_workspace_write.exclude_slash_tmp` — "Exclude `/tmp` from writable roots in
    workspace-write mode."
- CLI surface: `--sandbox <mode>`, `--ask-for-approval <policy>`, `-c/--config key=value`
  (value parsed as TOML), `/permissions` inside the TUI, `codex --full-auto` for the
  "Auto" preset. `--full-auto` is **`UNVERIFIED`** as to current status: a third-party blog
  refers to "The `--full-auto` Deprecation", but I did not load a vendor page saying so.

**Current/beta: named permission profiles** (`LOADED`, `codex-docs.com/en/docs/permissions`,
a mirror of `developers.openai.com/codex/permissions`):

- `default_permissions = ":read-only" | ":workspace" | ":danger-full-access" | "<name>"`.
  The built-ins are named in the vendor reference too: "Built-ins are `:read-only`,
  `:workspace`, and `:danger-full-access`; custom profile names require matching
  `[permissions.<name>]` tables. Don't combine with `sandbox_mode` or
  `[sandbox_workspace_write]`." (`MIRROR`)
- A profile is filesystem rules **and** network rules, not a rung:
  - `[permissions.<name>.filesystem]` — keys are absolute paths, globs, or the special tokens
    `:minimal`, `:tmpdir`, `:slash_tmp`, and nested `":workspace_roots"`; values are
    `"read" | "write" | "deny"` (`MIRROR`, reference).
  - `[permissions.<name>.workspace_roots]` — "Profile-defined workspace roots that receive
    `:workspace_roots` filesystem rules alongside the session's runtime workspace roots."
  - `[permissions.<name>.network] enabled = true|false`, plus `network.domains` as a map of
    host/glob → `"allow" | "deny"`, `network.unix_sockets` as socket path →
    `"allow" | "deny"`, `network.mode = "limited" | "full"`, `network.proxy_url`,
    `network.allow_local_binding` (`MIRROR`).
  - `[permissions.<name>] extends = ":read-only" | ":workspace" | "<name>"` — "optional parent
    profile applied before this named profile"; `:danger-full-access`, undefined parents and
    cycles are rejected.
- Top-level `features.network_proxy` (bool): "Start the network proxy for sandboxed commands
  (experimental; off by default). **Required to enforce permission-profile domain rules**
  unless enabled administrator-managed `experimental_network` requirements start the proxy."
  And in the permissions page: "a profile's `network.enabled = true` permits command network
  access, but it does not start the network proxy … Without an active proxy, profile domain
  rules do not restrict direct network access."
- Managed requirements (`requirements.toml`): `allowed_permission_profiles` (map name → bool;
  omitted profiles are denied, including built-ins added in future versions),
  `allowed_sandbox_modes`, `allowed_approval_policies`, `allowed_approvals_reviewers`,
  `guardian_policy_config`, `[experimental_network]`, `[permissions.filesystem] deny_read`,
  `[[remote_sandbox_config]] hostname_patterns` (per-host sandbox policy).

### (b) How the axes combine

- Legacy axes are orthogonal by design: "**Sandbox mode**: What Codex can do technically …
  **Approval policy**: When Codex must ask you before it executes an action." (`LOADED`)
- The combination is a preset ladder, not a matrix of free values. The docs give the "Auto"
  preset as `--sandbox workspace-write --ask-for-approval on-request`, and the read-only
  recipe as `sandbox_mode = "read-only"` + `approval_policy = "on-request"` (`LOADED`). A
  third-party article reproduces the legacy table (`MIRROR`/secondary):

  | Mode | Filesystem | Network |
  |---|---|---|
  | `read-only` | no writes | disabled |
  | `workspace-write` | writes to CWD + `writable_roots` | disabled, opt-in via `network_access` |
  | `danger-full-access` | unrestricted | unrestricted |

- Network is a separate boolean inside the middle rung, and it defaults off: "By default, the
  agent runs with network access turned off."
- The two generations do not mix: "Permission profiles do not compose with the older sandbox
  settings. Configure either `default_permissions` and `[permissions]`, or `sandbox_mode` /
  `sandbox_workspace_write`, but not both."
- Managed `deny_read` overrides the ladder: "When deny-read requirements are present, the
  local runtime rejects full-access permissions and keeps local execution in a read-only or
  workspace sandbox so it can enforce them."

### (c) Temporary / per-call / per-host grants

- **Per-call escalation is a model-initiated request.** "When it needs to cross the sandbox
  boundary, it requests approval" (`LOADED`); the prompt template file
  `codex-rs/prompts/templates/permissions/approval_policy/on_request_rule_request_permission.md`
  exists in the repo (`LOADED` — page loaded; body not readable through the blob view).
  Auto-review's trigger list names "Shell or exec tool calls that **request escalated sandbox
  permissions**" and "File edits outside the allowed writable roots" (`LOADED`).
- **A reviewer agent can absorb the prompt**: `approvals_reviewer = "auto_review"` routes the
  approval to a second Codex agent. "Auto-review is a reviewer swap, not a permission grant.
  It does not expand `writable_roots`, enable network access, or weaken protected paths."
- **One exact denied action can be approved for one retry**: in the TUI, `/approve` opens an
  "Auto-review Denials" picker; "That approval is narrow: it applies to the exact denied
  action, not similar future actions; it is recorded for one retry in the same context; and
  the retry still goes through Auto-review."
- **Rejection circuit breaker**: "Auto-review interrupts the turn after `3` consecutive
  denials or `10` denials within a rolling window of the last `50` reviews in the same turn."
- **Per-host policy**: `[[remote_sandbox_config]] hostname_patterns` gives a different
  `allowed_sandbox_modes` on matched hosts — the only per-host grant I found anywhere.
- **Per-path, persistent**: `writable_roots` (config), workspace roots (profiles). No
  time-bounded or use-count-bounded grant exists; the request for one was closed
  `not_planned` (see §6).

### (d) Verbatim quotes

1. "Codex security controls come from two layers that work together" — `LOADED`,
   https://www.codex-docs.com/en/docs/agent-approvals-security
2. "For a middle ground, `approval_policy = { granular = { ... } }` lets you keep specific
   approval prompt categories interactive while automatically rejecting others." — mirror of
   the Codex manual; the vendor reference confirms the value exists ("Allowed approval
   policies, such as `on-request`, `never`, and `granular`"), but the four category names
   (`sandbox_approval`, `rules`, `mcp_elicitations`, `request_permissions`) are `UNVERIFIED`.
3. "Auto-review is a reviewer swap, not a permission grant." — `LOADED`,
   https://www.codex-docs.com/en/docs/sandboxing/auto-review
4. "In practice, the highest-leverage changes are: Add narrow `writable_roots` for scratch
   directories or neighboring repos you intentionally use. Add narrowly scoped prefix rules …
   Broad rules often erase the very boundary Auto-review is meant to guard." — `LOADED`, same
   page (the maintainer's own advice is to widen the boundary rather than teach the reviewer
   to wave things through)

### (e) Sources

- https://developers.openai.com/codex/security — official; **403 to this fetcher**
- https://developers.openai.com/codex/config-reference — official; **403 to this fetcher**
- https://developers.openai.com/codex/permissions — official (linked from the mirror); not
  loaded directly
- https://www.codex-docs.com/en/docs/agent-approvals-security — `LOADED` (mirror)
- https://www.codex-docs.com/en/docs/permissions — `LOADED` (mirror)
- https://www.codex-docs.com/en/docs/sandboxing/auto-review — `LOADED` (mirror)
- https://www.codex-docs.com/en/docs/config-file/config-advanced — `LOADED` (mirror)
- https://www.codex-docs.com/en/docs/enterprise/managed-configuration — `LOADED` (mirror)
- https://github.com/openai/codex/blob/main/docs/config.md — `LOADED`; now a stub that
  redirects to developers.openai.com
- https://github.com/openai/codex/blob/main/docs/sandbox.md — `LOADED`; same, a stub
- https://github.com/openai/codex/pull/2464 — `LOADED` (title/body visible)
- https://github.com/openai/codex/commit/4668feb43a7ab2becf357765c538c70a2043b4e1 —
  `chore(core) Deprecate approval_policy: on-failure (#11631)`
- Mirror of the config reference actually read:
  https://cdn.jsdelivr.net/gh/mehmetbaykar/codex-docs-skill@main/skills/codex-docs/references/config-file__config-reference.md
  (front matter: `source: https://learn.chatgpt.com/docs/config-file/config-reference`)

**[UNVERIFIED] for Codex:** the exact prose of the `sandbox_mode` and `approval_policy` entries
(reflects a page that renders its body with JS); the granular-policy sub-keys; whether
`--full-auto` still exists.

---

## 2. Claude Code (Anthropic)

### (a) Knobs and values

**Permission modes** — six, not three (`LOADED`, `code.claude.com/docs/en/permission-modes`):

| Mode | What runs without asking |
|---|---|
| `default` (label "Manual") | "Reads only" |
| `acceptEdits` | "Reads, file edits, and common filesystem commands (`mkdir`, `touch`, `mv`, `cp`, etc.)" |
| `plan` | "Reads, plus classifier-approved commands when auto mode is available" |
| `auto` | "Everything, with background safety checks" |
| `dontAsk` | "Reads and pre-approved tools; anything that would prompt is denied" |
| `bypassPermissions` | "Everything" |

Modes are switchable at runtime (Shift+Tab, VS Code indicator, Desktop selector); the CLI also
accepts `manual` as an alias for `default`, and `claude --permission-mode <value>` sets it.

**Rule syntax** (`permissions.allow` / `permissions.ask` / `permissions.deny`), as the task
described — `Bash(npm run test:*)`, `Read(...)`, `WebFetch(domain:...)`. The docs' own detail
worth stealing:

- "When you choose 'Yes, and don't ask again' and the approval saves permanently, such as for a
  Bash command or a WebFetch domain, Claude Code saves the rule to `.claude/settings.local.json`
  at the root of the git repository, **resolved through worktrees to the main checkout**."
- Lifetimes differ by tool: Bash and WebFetch approvals are "Permanently per repository";
  file-modification approvals last "Until session end" only.
- Deny-first precedence survives hooks: "a matching ask rule still prompts even when the hook
  returned `"allow"` … To run all Bash commands without prompts except for a few you want
  blocked, add `"Bash"` to your allow list and register a PreToolUse hook that rejects those
  specific commands."

**Extra directories**: `--add-dir <path>` at startup, `/add-dir` in session,
`permissions.additionalDirectories` in settings — "Files in additional directories follow the
same permission rules as the original working directory: they become readable without prompts,
and file editing permissions follow the current permission mode." A real distinction is
documented between the flag and the settings key: flag-added directories also load
configuration (skills, commands, subagents) and are re-applied on `/cd`; settings-file
directories "grant file access only". There is also
`permissions.blockReadsOutsideWorkingDirectories`, and a first-read-outside-workspaces prompt.

**OS sandbox** (`LOADED`, `sandboxing` page; engineering post):

- macOS Seatbelt ("nothing to install"), Linux/WSL2 bubblewrap; "Native Windows is not
  supported."
- `/sandbox` panel with Mode / Overrides / Config tabs; `allowUnsandboxedCommands` is the
  escape-hatch setting.
- Default writable set: "the working directory, the session temp directory, and any directories
  you've added with `--add-dir`, `/add-dir`, or `permissions.additionalDirectories`."
- `sandbox.filesystem.allowWrite` for paths outside that set (`~/.kube`, `/tmp/build` are the
  docs' example).
- **Network** is a domain allowlist with a prompt on first use: "The first time a command needs
  a new network domain, Claude Code prompts for approval"; in auto mode the command carries its
  needed hosts for the classifier.
- **Unsandboxed fallback is labelled, not silent**: "Claude Code titles their permission prompt
  'Bash command (unsandboxed)' instead of 'Bash command', so you can tell which commands ran
  outside the sandbox." And the escape hatch is agent-visible: "Claude Code includes an escape
  hatch: Claude analyzes the violation and may retry the command with the
  `dangerouslyDisableSandbox` parameter."
- The engineering post's rationale and number: "In our internal usage, we've found that
  sandboxing safely reduces permission prompts by 84%." It also names the failure mode this
  whole design area exists to fix: "Constantly clicking 'approve' slows down development cycles
  and can lead to 'approval fatigue', where users might not pay close attention to what they're
  approving."

**Auto mode** (the classifier) is its own permission mode, not a rung: a second model reviews
actions; "Repeated blocks: if the classifier blocks an action 3 times in a row or 20 times
total, auto mode pauses and Claude Code resumes prompting. Approving the prompted action
resumes auto mode. These thresholds are not configurable." The default starting mode is auto on
Pro/Max/Team.

### (b) How the axes combine

Three separable layers, which is the interesting part:

1. **Mode** — what may run without asking (`default` … `bypassPermissions`).
2. **Rules** — `allow` / `ask` / `deny` per tool and per argument pattern; deny and ask are
   evaluated regardless of hooks, and project `allow` rules wait for the workspace-trust dialog.
3. **Sandbox** — OS-level filesystem and network bounds for Bash/PowerShell/Monitor and their
   children. Sandbox modes are `auto-allow` vs `regular permissions` (whether sandboxed
   commands skip the prompt), which is orthogonal to the permission mode.

The engineering post argues the two sandbox halves must both be present: "Without network
isolation, a compromised agent could exfiltrate sensitive files like SSH keys; without
filesystem isolation, a compromised agent could easily escape the sandbox and gain network
access."

### (c) Temporary / per-call grants

- **One-time / session / permanent** at the prompt: the docs distinguish approvals that are
  "saved permanently" (Bash command, WebFetch domain → repo-scoped file) from a
  file-modification approval that "isn't saved to the file: as the table shows, it lasts until
  the session ends". A prompt offers no "don't ask again" option when it cannot show the user
  everything the rule would allow — a good rule to steal.
- **Per-directory**, at three lifetimes: flag (this session), `/add-dir` (this session),
  settings key (persistent).
- **Per-command unsandboxed retry**: `dangerouslyDisableSandbox` on a single call, with a
  differently-titled prompt.
- **Auto-review thresholds** pause the classifier back into prompting (3 / 20).
- **Hooks** (`PreToolUse`, `PermissionRequest`) are the general custom-policy escape valve.

### (d) Verbatim quotes

1. "Instead of approving each command, you define which files and network domains commands can
   touch, and the operating system enforces that boundary for every Bash, PowerShell, or
   Monitor command and its child processes." — `LOADED`,
   https://code.claude.com/docs/en/sandboxing.md
2. "The first time a command needs a new network domain, Claude Code prompts for approval." —
   `LOADED`, same page
3. "Claude Code titles their permission prompt 'Bash command (unsandboxed)' instead of 'Bash
   command', so you can tell which commands ran outside the sandbox." — `LOADED`, same page
4. "In our internal usage, we've found that sandboxing safely reduces permission prompts by
   84%." — `LOADED`, https://www.anthropic.com/engineering/claude-code-sandboxing (Oct 20,
   2025)
5. "Each mode makes a different tradeoff between convenience and oversight." — `LOADED`,
   https://code.claude.com/docs/en/permission-modes.md

### (e) Sources

- https://code.claude.com/docs/en/permission-modes.md — `LOADED`
- https://code.claude.com/docs/en/permissions.md — `LOADED`
- https://code.claude.com/docs/en/sandboxing.md — `LOADED`
- https://code.claude.com/docs/en/settings-reference.md — `LOADED` (key index)
- https://code.claude.com/docs/en/cli-reference.md — `LOADED`
- https://www.anthropic.com/engineering/claude-code-sandboxing — `LOADED`
- https://github.com/anthropic-experimental/sandbox-runtime — linked from the post as the open
  source runtime; not loaded
- https://code.claude.com/docs/en/sandbox-environments — linked; not loaded
- https://github.com/anthropics/claude-code/issues/39523 — `LOADED` (title + lede)

---

## 3. Gemini CLI

Full detail came from a delegated research pass that loaded the repo docs; its source URLs are
reproduced unedited. Where I did not load the page myself it is marked.

### (a) Knobs and values

- `--approval-mode <default | auto_edit | yolo | plan>` (default `default`).
  `-y`/`--yolo` still exists but is deprecated in favour of `--approval-mode=yolo`.
  `general.defaultApprovalMode` accepts only `default`, `auto_edit`, `plan` — YOLO is
  CLI-only. Shift+Tab cycles Default → Auto-Edit → Plan.
  - **Correction to a common claim:** PR #26714 ("merge Auto modes into a single Auto mode")
    is about **model routing**, not permissions. There is no `auto` approval mode.
- `--sandbox` / `-s` is a **boolean** flag. The values live on `GEMINI_SANDBOX`
  (`true|false|docker|podman|sandbox-exec|runsc|lxc`, or a custom command string) and on
  `tools.sandbox` (bool | profile path | command string | `{command, image}`). macOS also has
  `SEATBELT_PROFILE` with `permissive-open` (default) … `strict-proxied`. Related settings:
  `security.toolSandboxing`, `tools.sandboxAllowedPaths`, `tools.sandboxNetworkAccess`.
- Tool gating: `tools.core` (allowlist over **all** built-in tools), `tools.allowed` (bypass
  confirmation), `tools.confirmationRequired` ("Takes precedence over allowed tools and core
  tool allowlists"), `tools.exclude` (deprecated). `--allowed-tools` is deprecated in favour
  of the **policy engine**.
- **Policy engine** (`~/.gemini/policies/*.toml`): fields `toolName`, `subagent`, `mcpName`,
  `toolAnnotations`, `argsPattern`, `commandPrefix`, `commandRegex`,
  `decision = "allow" | "deny" | "ask_user"`, `priority` 0–999, `denyMessage`, `modes`,
  `interactive`, `allowRedirection`.

### (b) How the axes combine

The policy engine, not the approval mode, is the arbiter: tiers Default 1 / Extension 2 /
Workspace 3 (currently disabled) / User 4 / Admin 5, with
`final_priority = tier_base + toml_priority/1000`, and "the rule with the highest priority
wins" — *not* "deny always beats allow". YOLO is itself implemented as a Default-tier
allow-all rule at `priority = 998` → 1.998, so any user-tier rule (4.x, which includes
"Always Allow" at 4.95 and `--exclude-tools` at 4.4) outranks it. Two built-in exceptions stay
above YOLO: `ask_user` remains `ask_user`, and `enter_plan_mode`/`exit_plan_mode` are denied.
Folder trust overrides everything: in an untrusted workspace "You will always be prompted
before any tool is run, even if you have auto-acceptance enabled globally."

### (c) Temporary / per-call grants

- Confirmation dialog labels (PR #15296): "Allow once", "Allow for this session", "Allow for
  all future sessions".
- Persisted "always allow" is mode-context aware, with the hierarchy
  `plan < default < autoEdit < yolo`: a grant made in `default` covers `default`, `autoEdit`
  and `yolo`; one made in `yolo` covers only `yolo`.
- **Sandbox expansion request**: "If you approve the expansion, the command is executed with
  the extended permissions for that specific run."
- MCP: proceed once / always allow this tool / always allow this server; `trust: true`.
- `gemini mcp enable|disable <name> --session`; `--skip-trust` trusts the workspace for the
  session.
- Note: `gemini mcp disable` is the only tool where the current docs name a genuine
  session-scoped toggle.

### (d) Verbatim quotes

1. "The policy engine uses a sophisticated priority system to resolve conflicts when multiple
   rules match a single tool call. The core principle is simple: the rule with the highest
   priority wins." — https://geminicli.com/docs/reference/policy-engine/
2. "Sandbox is enabled when using `--yolo` or `--approval-mode=yolo` by default." —
   https://github.com/google-gemini/gemini-cli/blob/main/docs/reference/configuration.md
3. "Approval: If you approve the expansion, the command is executed with the extended
   permissions for that specific run." —
   https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/sandbox.md
4. "The `tools.core` setting is an allowlist for _all_ built-in tools, not just shell
   commands." — https://github.com/google-gemini/gemini-cli/blob/main/docs/tools/shell.md

### (e) Sources

- https://geminicli.com/docs/reference/policy-engine/
- https://github.com/google-gemini/gemini-cli/blob/main/docs/reference/configuration.md
- https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/cli-reference.md
- https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/sandbox.md
- https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/trusted-folders.md
- https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/settings.md
- https://github.com/google-gemini/gemini-cli/blob/main/docs/tools/shell.md
- https://github.com/google-gemini/gemini-cli/blob/main/packages/core/src/policy/policies/yolo.toml
- https://github.com/google-gemini/gemini-cli/pull/15296 (confirmation labels)
- https://github.com/google-gemini/gemini-cli/pull/18508 (deprecate `--allowed-tools`)
- https://github.com/google-gemini/gemini-cli/pull/18183 (per-command scopes — **closed,
  unmerged**)

**[UNVERIFIED]:** `write.toml` / `read-only.toml` / `plan.toml` priorities; the exact
`--yes`/`--yes-always`-style env var names; whether `--exclude-tools` is user-facing (it
appears only in a source comment); a docs-site banner claiming Gemini CLI "was replaced by
Antigravity CLI on June 18th, 2026" for unpaid users.

---

## 4. Cursor CLI

### (a) Knobs and values (`LOADED`)

- `permissions.allow` / `permissions.deny` — arrays of **permission tokens**:
  - `Shell(commandBase)` — first token of the command line, globs and an optional
    `command:args` form: `Shell(ls)`, `Shell(git)`, `Shell(curl:*)`, `Shell(rm)`.
  - `Read(pathOrGlob)`, `Write(pathOrGlob)` — globs, e.g. `Read(src/**/*.ts)`,
    `Write(package.json)`, and as deny entries `Read(.env*)`, `Write(**/*.key)`.
  - `WebFetch(domainOrPattern)` — `WebFetch(docs.github.com)`, `WebFetch(*)`; "Without an
    allowlist entry, each fetch prompts for approval."
  - `Mcp(server:tool)` — `Mcp(datadog:*)`, `Mcp(*:*)`.
- Files: `~/.cursor/cli-config.json` (global) and `<project>/.cursor/cli.json` (project). Per a
  delegated pass: "Only permissions can be configured at the project level. All other CLI
  settings must be set globally."
- `approvalMode` (per the delegated pass) = `allowlist` | `auto-review` | `unrestricted`;
  CLI flags `-f/--force` ("Force allow commands unless explicitly denied"), `--yolo` ("Alias
  for `--force`"), `--sandbox enabled|disabled`, `--approve-mcps`, `--trust`,
  `--mode plan|ask`.
- Run Modes (Cursor 3.6+, `LOADED` from the help page): **Auto-review** (default), **Allowlist**,
  **Run Everything**; before 3.5 they were **Run in Sandbox**, **Ask Every Time** (deprecated),
  **Run Everything**.
- Sandbox: macOS Seatbelt, Linux Landlock + seccomp; `agent sandbox enable|disable|reset|run`
  with per-invocation `--allow-paths`, `--readonly-paths`, `--blocked-patterns`, `--network`.
  A separate `sandbox.json` (delegated pass) with `type` =
  `workspace_readwrite` (default) | `workspace_readonly` | `insecure_none`,
  `additionalReadwritePaths`, `additionalReadonlyPaths`, `networkPolicy.default` +
  `allow[]`/`deny[]`.

### (b) How the axes combine

"Sandboxing is a layer on top of Run Modes for shell commands. It controls **where** a supported
terminal command runs, not **whether** the mode uses the Auto-review classifier." Deny beats
allow: "Deny rules take precedence over allow rules." `--force` widens but does not erase
explicit denies.

### (c) Temporary / per-call grants

- Interactive per-command y/n; per-invocation `--yolo`/`--force`; `agent sandbox run <cmd>`
  with one-shot path/network flags; `--trust` for one headless run. A session-scoped
  "always allow this command" was **not found** in any page loaded.

### (d) Verbatim quotes

1. "Before running terminal commands, CLI will ask you to approve (y) or reject (n)
   execution." — https://cursor.com/docs/cli/using.md
2. "Deny rules take precedence over allow rules" —
   https://cursor.com/docs/cli/reference/permissions.md
3. "Sandboxing is a layer on top of Run Modes for shell commands. It controls where a supported
   terminal command runs, not whether the mode uses the Auto-review classifier." —
   https://cursor.com/docs/agent/security/run-modes.md
4. "**Auto-review**. Allowlisted commands run, the rest run in the sandbox when possible, and
   anything else is screened by an LLM classifier that allows or blocks based on safety and how
   well the command matches your request." — `LOADED`,
   https://cursor.com/help/ai-features/terminal.md

### (e) Sources

- https://cursor.com/docs/cli/reference/permissions — `LOADED`
- https://cursor.com/docs/cli/reference/parameters — `LOADED`
- https://cursor.com/help/ai-features/terminal.md — `LOADED`
- https://cursor.com/docs/cli/reference/configuration.md, /using.md,
  /docs/agent/security/run-modes.md, /docs/reference/sandbox.md — loaded by the delegated pass

**[UNVERIFIED]:** the JSON enum strings for `sandbox.mode` / `sandbox.networkAccess`; whether a
persistent "always allow" prompt exists.

---

## 5. Amp, OpenCode, Aider

### Amp

- **Current docs have no approval system.** `ampcode.com/manual` now redirects to
  `ampcode.com/docs`, and `/docs/tools` says: "By default, Amp does not ask for approval before
  running tools." The permissions section of that page reads: "Amp does not ask for approval
  before running tools. Use a custom plugin to control tool use."
- Gating today is a Ts/JS **plugin** hook: "`tool.call` fires before a tool runs. Return
  `allow` to run the tool, `reject-and-continue` to block it and let the agent continue,
  `modify` to change the input, or `synthesize` to provide a result without running the tool."
  Plugins can call `ctx.ui.confirm(...)`, `ctx.ui.select(...)`, and `amp.ai.ask(...)`.
- Remaining settings-level knobs: `amp.tools.disable` (array, glob-capable) and
  `amp.mcpPermissions` (array of `{matches, action: "allow" | "reject"}`; "Amp applies the
  first matching rule. If no rule matches an MCP server, Amp allows it.").
- **Legacy, still online but orphaned (do not cite as current):** `amp.permissions` with
  entries matched on tool + argument patterns and one of four actions. The Aug 7 2025 news post
  says "The matched entry then tells Amp to either _allow_ the tool call, _reject_ it, _ask_ you
  for confirmation, or _delegate_ the decision to another program." That four-way action set —
  allow / ask / reject / **delegate to an external program** — is still the most interesting
  idea in Amp, even though its documented home is gone.
- **`--dangerously-allow-all` is `UNVERIFIED`** — it appears on no page loaded in this session.
- Sources: https://ampcode.com/docs/tools#permissions, https://ampcode.com/docs/cli/settings,
  https://ampcode.com/docs/customize/plugins, https://ampcode.com/news/tool-level-permissions,
  https://ampcode.com/notes/permissions

### OpenCode

- `permission` in `opencode.json`, three values per rule: `"allow"` (run without approval),
  `"ask"` (prompt), `"deny"` (block). Three shapes: one scalar; per-tool scalars; and granular
  per-tool pattern maps.
- "Rules are evaluated by pattern match, with the **last matching rule winning**. A common
  pattern is to put the catch-all `"*"` rule first, and more specific rules after it."
- Keys: `read`, `edit`, `glob`, `grep`, `bash`, `task`, `skill`, `lsp`, `question`, `webfetch`,
  `websearch`, `external_directory`, `doom_loop`. Defaults: most `"allow"`;
  `doom_loop` and `external_directory` default `"ask"`; `read` allows all except `*.env`,
  `*.env.*` (with `*.env.example` allowed).
- Pattern detail worth copying: "`grep *` allows `grep pattern file.txt`, while `grep` alone
  would block it"; `~`/`$HOME` expand at the start of a pattern; and crucially — "Home expansion
  … does not make an external path part of the current workspace, so paths outside the working
  directory must still be allowed via `external_directory`."
- One flag: `--auto` ("Auto-approve permissions that are not explicitly denied"). "Explicit
  `"deny"` rules are still enforced. Auto mode only changes requests that would otherwise ask
  for approval." **No `--yolo` is documented.**
- Session-scoped grant, named in the UI contract: "`once` — approve just this request;
  `always` — approve future requests matching the suggested patterns (**for the rest of the
  current OpenCode session**); `reject` — deny" — and the *tool* suggests the pattern: "The set
  of patterns that `always` would approve is provided by the tool (for example, bash approvals
  typically whitelist a safe command prefix like `git status*`)."
- No OS sandbox is documented; gating is in-process on parsed command strings and paths.
- Sources: https://opencode.ai/docs/permissions/ (`LOADED` by me),
  https://opencode.ai/docs/cli/, https://opencode.ai/docs/policies/,
  https://github.com/anomalyco/opencode

### Aider

- **No permission system, no allow/deny list, no approval mode, no sandbox.** The only
  confirmations to suppress are Aider's own (`--yes-always`, sample config comment "Always say
  yes to every confirmation"), and the only commands it runs on its own are declared up front:
  `--lint-cmd` (+ `--auto-lint`, default true), `--test-cmd` (+ `--auto-test`, default false).
  Shell execution otherwise is user-initiated: `/run`, `/test`.
- File edits are never gated once a file is in the chat; `--read` marks a file read-only for
  Aider's purposes, `--dry-run` suppresses writes, and `--auto-commits` (default true) is the
  undo story.
- Flag-name caveat: the scripting page still documents `--yes`; a PR renaming it to
  `--yes-always` exists, so treat the env var name as `UNVERIFIED`.
- Sources: https://aider.chat/docs/config/options.html (`LOADED`),
  https://aider.chat/docs/config/aider_conf.html (`LOADED`),
  https://aider.chat/docs/usage/commands.html, https://aider.chat/docs/scripting.html,
  https://github.com/Aider-AI/aider/pull/5551

---

## 6. Criticism of the coarse three-mode ladder

Two distinct failure directions; keep them apart in the design doc.

### 6.1 The middle rung is too tight for real work

- **openai/codex#2444** "Need ability to whitelist ~/.cache in safe sandbox mode" — alexchandel,
  2025-08-19: "Codex gets confused when it can't access `~/.cache` and frequently doesn't
  recognize the errors `uv` throws because of this, and begins running unpredictable hacky
  commands."
  https://github.com/openai/codex/issues/2444
- **openai/codex#1457** "Python UV fails in Codex" — corrylc, 2025-07-03: "In the rust version
  it seems that all modes produce the same sandboxing errors, making the tool functionally
  unusable in the present state." and "I tried adding the `~/.cache/uv` directory to writable
  roots, but it had no apparent effect." (47 👍)
- **openai/codex#3141** "Allow GPU access inside sandbox" — enolan, 2025-09-04, open: "There
  should be a way to allow Codex to use GPUs without completely disabling the sandbox. I want
  it to be able to run tests for my ML projects." (61 👍)
- **openai/codex#10390** — fede-kamel, 2026-02-02, open (15 👍): the macOS seatbelt sandbox
  ignores the documented middle-rung network toggle; the stated workaround is
  `codex --sandbox danger-full-access`.
- **openai/codex#10797** — Penlect, 2026-02-05: with `network_access=false`, even *local*
  AF_UNIX connections fail (`xdotool` → `/tmp/.X11-unix/X0`). Network-off is not just "no HTTP".
- **openai/codex#23601** — maizaAvaro, 2026-05-20: "This makes the sandbox effectively unusable
  and turns normal development workflow into repeated escalation prompts."
- **openai/codex#30712** — PurpleDevX, 2026-06-30, open (14 👍), the strongest single artifact:
  "The safe edit path (`apply_patch`) is unusable in Codex desktop app sessions on Windows.
  Agents have to fall back to shell-based file rewrites inside the project, which bypasses the
  sandbox and is less precise and more error-prone." Error text quoted in the report:
  "windows unelevated restricted-token sandbox cannot enforce split writable root sets
  directly; refusing to run unsandboxed".
  https://github.com/openai/codex/issues/30712
- **anomalyco/opencode#18441** — ChrisFloofyKitsune, 2026-03-20, the cleanest statement of the
  problem from a user's side: "This makes it impossible to achieve the common use case of
  'read-only access to external directories' — you must choose between: `"allow"` → full
  read+write access (too permissive); `"ask"` → prompted on every read (too noisy); `"deny"` →
  no access at all (too restrictive)."

### 6.2 People end up on full access anyway, and full access still prompts

- **openai/codex#30712** comment — peancor, 2026-07-24: "In neither sandbox mode provided a
  functional workflow, I knowingly accepted the risks of Full access." and "Starting today,
  even with **Full access** selected, Codex repeatedly asks me to approve commands—often
  roughly once per minute while an agent is working."
- **openai/codex#14547** — arvinmi, 2026-03-13: prompts still appear "even though
  `approval_policy = "never"` is set … `sandbox_mode = "workspace-write"` … and codex is
  launched with `--dangerously-bypass-approvals-and-sandbox (--yolo)`".
- **openai/codex#19196** — agjones, 2026-04-23 (21 👍): "'Full Access' permissions broken.
  Network calls are still sandboxed when these permissions are active."
- **anthropics/claude-code#39523** — interconnectedMe, 2026-03-26, open (18 👍), a meta-issue
  over 12+ duplicates: "**`bypassPermissions` should mean bypass.** If I've opted into the mode
  with the scary name, I've accepted the risk."
  https://github.com/anthropics/claude-code/issues/39523
- **openai/codex#16911** — codengine, 2026-04-06: "It is really annoying that I have to approve
  tool calls continuously even though they already have been approved (Allow always). … I don't
  want to nanny Codex for every second tool call?"
- **openai/codex#22181** — rsamuel-circle, 2026-05-11: "Approval-fatigued users habitually pick
  `(p)` to silence prompts." — i.e. fatigue *manufactures* over-broad, program-name-scoped,
  cross-session grants.
- **openai/codex#29235** — mrlightsource-create, 2026-06-20, open (21 👍): "Codex repeatedly
  asks the user for permission before ordinary actions even when the thread is configured with
  full filesystem access and approval prompts disabled."
- Blog: https://wmedia.es/en/tips/claude-code-auto-mode-vs-yolo (Apr 25 2026): "If you live
  with `--dangerously-skip-permissions` in Claude Code and haven't tried auto mode yet, you're
  missing most of YOLO's flow with an actual safety net." and "I used YOLO via a `claude-yolo`
  alias for months."
- Blog (Chinese, 2026-08-25): https://blog.csdn.net/jiangjunshow/article/details/161400789 —
  first-hand count of 72 approvals in one day.

### 6.3 Maintainers/vendors adding a middle ground — the evidence that the ladder was too coarse

- **The `writable_roots` chain, all on 2025-08-19.** The complaint (#2444, 03:56Z) →
  maintainer **bolinfest** answers (04:48Z): "Oops, this is not currently documented on
  …config.md#sandbox_mode, but you can do this in your `config.toml` to include additional
  writable folders: `sandbox_workspace_write.writable_roots = ["/Users/YOU/.cache"]`" → PR
  **#2464** "docs: document writable_roots for sandbox_workspace_write" merged 18:39Z, body:
  "As discovered on #2444, this was missing from the docs." Three linkable steps, one day.
  https://github.com/openai/codex/issues/2444#issuecomment-3199192844 ·
  https://github.com/openai/codex/pull/2464
- **Codex's own docs now admit the ladder is not enough**: "For a middle ground,
  `approval_policy = { granular = { ... } }` lets you keep specific approval prompt categories
  interactive while automatically rejecting others." (mirror; the *shape* is confirmed by the
  vendor reference listing `granular` among allowed approval policies)
- **Codex added a reviewer agent to absorb prompt pressure**: `approvals_reviewer =
  "auto_review"`, with the maintainer advice "If too many mundane actions need review, fix the
  boundary first instead of teaching the reviewer to approve noisy escalations forever."
- **Anthropic went from three modes to six**, and the docs justify the new one with the failure
  it fixes: the `auto` row reads "Everything, with background safety checks — Best for: Long
  tasks, **reducing prompt fatigue**". The same docs then document that even that mode falls
  back into prompting (3 consecutive / 20 total blocks).
- **Claude Code's sandbox settings are the per-tool middle ground**: `sandbox.filesystem.allowWrite`
  is "the recommended approach when a tool needs write access to a specific location, rather
  than excluding the tool from the sandbox entirely".
- **Gemini CLI needed a special case at the top rung**: PR #17920 (merged 2026-01-30) "fixes a
  bug where YOLO mode (auto-approval) incorrectly required manual confirmation for shell
  commands that failed to parse correctly (e.g., complex heredocs or process substitution)."
- **A user naming the missing primitive**: openai/codex#11176 — RichardoC, 2026-02-09: "We need
  a middle ground between 'always allow' and 'prompt every time.' Today, users either get stuck
  in repeated prompts or accept permanent access that's too risky for sensitive tools."
  (closed `not_planned` 2026-03-20) https://github.com/openai/codex/issues/11176

**Gaps in this section:** no Hacker News or Reddit citation survived — every attempt returned a
fetch failure or no thread body. Nothing here is attributed to Reddit/HN. `openai/codex#2464`
has zero comments, so there is no maintainer *discussion* to quote on it, only its body.

---

## 7. Ideas worth stealing (each one observed, with its home)

1. **Split the two axes and let the prompt name the difference.** Codex: sandbox = what is
   technically possible, approval policy = when it must ask. Claude Code adds a third: the
   sandbox's own auto-allow/regular distinction.
2. **A labelled unsandboxed fallback.** "Bash command (unsandboxed)" — the user can see, at the
   prompt, that this one call is different from the rest. Claude Code.
3. **The rule the prompt saves is only as wide as what the prompt could display.** Claude Code:
   a "don't ask again" option is offered only when `the prompt can show you everything they
   would allow`. This is the antidote to #22181's program-name-only grant.
4. **Per-path writable roots as a first-class config key, not a special case.**
   `sandbox_workspace_write.writable_roots` (Codex) and `--add-dir` /
   `additionalDirectories` (Claude Code); Codex's profile form generalises it to read / write /
   deny per path and per glob.
5. **Network as a boolean *inside* the middle mode, with a domain allowlist when the proxy is
   on.** Codex `network_access` + `permissions.<n>.network.domains`; Claude Code's
   first-use-per-domain prompt; Cursor `WebFetch(domain)` / `sandbox.json networkPolicy`.
6. **Tool-argument pattern rules, with an explicit conflict order.**
   `Bash(npm run test:*)`, `Shell(curl:*)`, `Mcp(datadog:*)` (Claude Code / Cursor),
   OpenCode's "last matching rule wins" with the catch-all first, Gemini's numeric priority
   (`priority` 0–999, tier base + priority/1000) with the built-in YOLO rule at 998 so any
   user rule outranks it.
7. **A session-scoped "always", distinct from "once" and from "forever".** OpenCode's
   `once` / `always` (rest of session) / `reject`, and — the clever part — *the tool supplies
   the pattern that "always" would cover* (`git status*`), rather than the UI inventing one.
   Gemini has the same three labels plus a mode-aware persistence rule (a grant made in
   `default` also covers `autoEdit` and `yolo`; one made in `yolo` covers only `yolo`).
8. **The pattern that gets saved remembers the mode it was granted in.** Gemini's
   `plan < default < autoEdit < yolo` hierarchy for "allow for all future sessions".
9. **A per-run permission expansion the model can request.** Gemini's Sandbox Expansion
   Request — granted "for that specific run" — and Codex's escalation request as one of
   auto-review's trigger categories.
10. **A reviewer agent as a *swappable reviewer*, explicitly not a grant.** Codex
    `approvals_reviewer = "auto_review"`: "It does not expand `writable_roots`, enable network
    access, or weaken protected paths." And the failure it guards against is documented:
    "If too many mundane actions need review, fix the boundary first."
11. **One denied action, one retry, explicitly narrow.** Codex `/approve` on the Auto-review
    Denials picker: applies to the exact denied action only, one retry, still reviewed.
12. **A circuit breaker on repeated denials.** 3 consecutive or 10-in-50 in a turn aborts the
    turn rather than letting the agent grind on escalations. Codex auto-review.
13. **Delegate the decision to an external program.** Amp's legacy `action: "delegate", "to":
    "<helper on $PATH>"` (with allow/ask/reject exit codes, `UNVERIFIED`) is the only design
    here that hands the policy to something the agent's authors did not write. Its documented
    home has since been removed from Amp's docs, so treat it as an idea, not a current
    implementation.
14. **Trust the workspace as an explicit, reviewable dialog that lists what the folder would
    grant.** Claude Code: project `allow` rules and `additionalDirectories` "grant capability",
    so they apply "only after you accept the workspace trust dialog for that folder. The dialog
    lists the rules and directories the folder would grant so you can review them first."
    Gemini's folder trust is the same idea and overrides the approval mode when untrusted.
15. **Per-host policy constraints.** Codex `[[remote_sandbox_config]] hostname_patterns` →
    different `allowed_sandbox_modes` on CI runners vs laptops ("Host name matching is for
    policy selection only; don't treat it as authenticated device proof" — a good honest
    caveat).
16. **Deny is not a rung; it is a rule that survives every rung.** Gemini's policy priorities,
    Cursor's "Deny rules take precedence over allow rules", Codex's managed `deny_read` forcing
    the runtime out of full access, Claude Code's deny/ask rules surviving hooks. A design that
    puts deny inside the mode ladder gets this wrong.
17. **Session-scoped resource toggles where resources are not commands.** Gemini
    `mcp enable|disable --session`; MCP server trust as an independent axis from command
    approval.
18. **Honest documentation of the limit.** Codex: "Auto-review improves the default operating
    point for long-running agentic work, but it is not a deterministic security guarantee."
