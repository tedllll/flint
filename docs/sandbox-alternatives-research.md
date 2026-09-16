# Capability- and scope-based alternatives to the permission ladder — research note

Research input for [`docs/sandbox.md`](sandbox.md) §3.4 / Stage 1 and Stage 2. Nothing here is
proposed for the tree; it is the reading that `sandbox.md` §7 lists as "not incorporated".
Delete or fold in as wanted.

Every quote below was read on the cited page. Markers:

- **[F]** read and quoted from the cited page during this research.
- **[P]** pointer only — the fetch failed or returned a JS shell, so the URL is given but the
  claim is not quoted from it.
- **[I]** inferred from the structure of a fetched primary source rather than stated in one
  sentence there.

---

## 1. Deno's permission flags

### (a) Mechanism and expressible scope

Flags are per *category*, and most categories take a scoped value list. **[F]**
<https://docs.deno.com/runtime/fundamentals/security/>,
<https://docs.deno.com/runtime/reference/permissions/>

- `--allow-read[=<PATH>...]` / `-R`, `--allow-write[=<PATH>...]` / `-W`. Paths are comma
  separated; "To include a comma character in the PATH, it must be doubled."
- `--allow-net[=<HOST>...]` / `-N`. "A host can be a hostname or IP address, optionally with a
  port." IPv6 is bracketed (`[2606:4700:4700::1111]`); a leading `*.` is the only wildcard, and
  "Hostnames do not allow subdomains, unless explicitly listed."
- `--allow-env[=<VAR>...]` / `-E`. Since Deno v2.1, suffix wildcards: `--allow-env="AWS_*"`.
- `--allow-run[=<PROGRAM>...]`, plus `--allow-sys`, `--allow-ffi`, `--allow-import`.
- `--deny-*` mirror every `--allow-*` and "take precedence over the allow flags", so a broad
  allow can be carved: `--allow-read --deny-read=/etc` reads anything except `/etc`.
- `-A` / `--allow-all`: "This **disables** the security sandbox entirely … The `--allow-all` has
  the same security properties as running a script in Node.js (ie none)."
- Extra knobs: `--ignore-env` (returns `undefined` instead of failing), `DENO_TRACE_PERMISSIONS=1`,
  `DENO_AUDIT_PERMISSIONS=<file>|otel` (JSONL audit of every permission access, allowed or denied).

### Denial and prompt behaviour

- Denial throws a catchable error **[F]**: "When an operation is refused (because the permission
  was not granted or was explicitly denied), Deno throws a `NotCapable` error." The CLI text is
  `error: Requires net access to "example.com", run again with the --allow-net flag`.
- There **is** a runtime prompt mode **[F]**:

  ```console
  ⚠️  Deno requests net access to "example.com". Run again with --allow-net to bypass this prompt.
     Allow? [y/n/A] (y = yes, allow; n = no, deny; A = allow all net permissions) >
  ```

  "When stdout is a terminal and you have not passed a flag, Deno pauses and asks instead of
  failing." Conversely: "Prompts are not shown if stdout/stderr are not a TTY, or when the
  `--no-prompt` flag is passed." A bare `--deny-read` "disabl[es] permission prompts for reads" —
  i.e. an explicit deny also switches the asking off.
- In-code equivalents **[F]**: `Deno.permissions.query`, `request` ("prompts for one on demand"),
  `revoke` ("downgrades a granted permission back to the prompt state").
- A **broker** mode replaces both flags and prompts **[F]**: with `DENO_PERMISSION_BROKER_PATH`
  set, "All `--allow-*` and `--deny-*` flags are ignored. Interactive permission prompts are not
  shown … Every permission check is sent to the broker; the broker must reply with a decision for
  each request." Any protocol error kills the process.

### (b) What it cannot express

1. **Per-module privilege.** This is Deno's own stated principle **[F]**: "All code executing on
   the same thread shares the same privilege level. It is not possible for different modules to
   have different privilege levels within the same thread." A grant given to a dependency is a
   grant to the whole process. (Workers can get a reduced set — the docs recommend exactly that
   for untrusted code — but that is process/thread-level subdivision, not per-package.)
2. **Nothing finer than the whole category when unscoped**, and the docs say so themselves
   **[F]**: "A bare `--allow-net` with no value grants the whole category, which is broader than
   most programs need."
3. **Subprocesses leave the model entirely** **[F]**: `--allow-run` runs "a separate program with
   its own permissions, not the restricted set you granted the Deno process, so whatever it does
   happens outside the sandbox"; `--allow-run=deno` "can start it with `--allow-all` … and
   escap[e] the sandbox entirely". `--allow-ffi` likewise: "a native library runs as compiled
   machine code in the same process and can issue system calls directly". The docs' verdict:
   "Treat both as equivalent to `--allow-all` when deciding whether to trust the code you are
   running."
4. **No per-call or expiring grants.** `request()` is per-call and `revoke()` narrows, but there
   is no "this host, for this task only" lifetime, and `A` at a prompt means "allow all net
   permissions" for the process.
5. **Deny is path/host-shaped, not effect-shaped.** The scope vocabulary is names and paths; there
   is no "read but not stat", no per-syscall view.

### (c) Documented complaints (verbatim)

- The project's own admission above ("broader than most programs need"; `--allow-run` ≡
  `--allow-all`) is the strongest documented trade-off statement in the official docs.
- Deno issue **#26839**, "I just had to remove all my fine grained permissions because of
  `--allow-run` changes in deno 2" **[F]** <https://github.com/denoland/deno/issues/26839>:

  > "I had an app that I upgraded from deno 1.x to 2.x … and I was very dismayed to find that I
  > had to replace *all of my fine grained permissions* with `--allow-all`, which I really hate."
  >
  > "So because I am calling a few subprocesses I now need to remove all of that for
  > `--allow-run`, which will allow *anything I import* to call out to any domain, read and write
  > any file on the whole operating system and invoke any child process… This is *insane*."

  The reporter's own proposed fix is a capability-scoped variant — per-module `--allow-run`,
  e.g. `--allow-run=./src/example.ts=git,gh,mkdir` — which is exactly the expressiveness the model
  lacks. (Maintainers closed it as completed; the direction was not adopted.)
- Deno issue **#35094** **[F]** <https://github.com/denoland/deno/issues/35094>: the nested
  permission objects (`Deno.test`, `WebWorker`) lag the CLI — scoped `--deny-*=`, `--ignore-read`,
  `--no-prompt` and `--permissions-set` "are starting to miss some newer flags", which "is
  starting to limit the permission model when not working in the main thread."
- The prompts themselves have a disable switch and a non-TTY silent mode, which is the usual sign
  that in practice they were being turned off rather than answered.

### (d) URLs

<https://docs.deno.com/runtime/fundamentals/security/> ·
<https://docs.deno.com/runtime/reference/permissions/> ·
<https://github.com/denoland/deno/issues/26839> ·
<https://github.com/denoland/deno/issues/35094>

---

## 2. Android / iOS / macOS TCC: permission-per-capability with a prompt

### (a) Mechanism and expressible scope

- **Android**: per-capability permissions, install-time in the original model and runtime-prompt
  from Android 6; Android 11 added one-time grants and automatic reset of unused apps' permissions
  **[P]** (<https://developer.android.com/about/versions/11/privacy/permissions> — this fetch
  returned only the JS page shell, so the wording of "Only this time" / auto-reset is not quoted
  here). The permission is the unit; the app is asked once per capability and the answer is
  remembered.
- **iOS**: runtime prompts on first use, per data type, with a small and fixed vocabulary of
  answers. Location is the notable one with three choices (Allow Once / Allow While Using App /
  Allow Always) **[P]** (<https://developer.apple.com/documentation/corelocation/cllocationmanager/requestwheninuseauthorization()>
  and Apple's user guide — both return JS shells to this fetcher). Photos added a partial-grant
  ("select photos") **[P]**. There is no per-request prompt for most capabilities: once granted,
  the answer is durable until the user changes it in Settings.
- **macOS TCC**: same shape — a per-capability record (Files and Folders, Full Disk Access,
  Accessibility, Automation, Screen Recording…), prompted on first use and thereafter answered
  from a database; `tccutil` resets it **[P]**.

### (b) What it cannot express

- **One-shot authority in general.** A prompt is answered once and the answer is kept. Android
  needed a *separate* feature (one-time permissions, auto-reset) to approximate the "this once"
  semantics that the model does not have.
- **Scope narrower than the capability.** "Camera" is the camera; there is no "this photo", no
  "this host", and no per-call resource identity. Apple's and Google's models are capability ×
  app, not capability × app × object.
- **No notion of delegating a capability onward** — an app cannot hand a specific permission to a
  helper process; the user is the only grantor and only through the OS UI.
- **Prompts are not decisions for most users.** The usability literature below is the evidence.

### (c) Prompt fatigue and "they approve everything" — the authoritative literature

**Felt, Ha, Egelman, Haney, Chin, Wagner, "Android Permissions: User Attention, Comprehension,
and Behavior", SOUPS 2012** **[F]** (abstract quoted by the authors' own institution, CMU CyLab,
which links the paper PDF):

> "Attention. In both the Internet survey and laboratory study, **17% of participants paid
> attention to permissions during a given installation**. At the same time, **42% of laboratory
> participants were unaware of the existence of permissions**. Comprehension. Overall,
> participants demonstrated very low rates of comprehension. **Only 3% of Internet survey
> respondents could correctly answer three comprehension questions.** … Our findings indicate that
> the Android permission system is neither a total success nor a complete failure."
>
> — <http://www.cyblog.cylab.cmu.edu/2012/07/cylabs-soups-continues-its-ongoing.html>; paper:
> <http://cups.cs.cmu.edu/soups/2012/proceedings/a3_Felt.pdf>

**Wijesekera, Baokar, Hosseini, Egelman, Wagner, Beznosov, "Android Permissions Remystified: A
Field Study on Contextual Integrity", USENIX Security 2015** — the canonical statement of
habituation **[F]** <https://www.usenix.org/conference/usenixsecurity15/technical-sessions/presentation/wijesekera>
(full text: <https://ar5iv.labs.arxiv.org/html/1504.03747>):

> "If users are asked to make security decisions **too frequently and in benign situations, they
> may become habituated and approve all future requests without regard for the consequences**. If
> they are asked to make too few security decisions, they may become concerned that the platform
> is revealing too much sensitive information."

and its measured result, over 36 participants, 27M logged permission accesses in one week:

> "We found out that **at least 80% of our participants would have preferred to prevent at least
> one permission request**, and overall, they thought that **over a third of requests were
> invasive** and desired a mechanism to block them."
>
> "fewer than one quarter of all permission requests (24.9% of 27M) occurred when the user had
> clear indications that those applications were running … **60% of permission requests occurred
> while participants' phone screens were off**."

The same paper records the design conclusion the literature converges on — prompt *less*, at
moments with context, and make refusals possible:

> "Reducing the number of security decisions a user must make at install-time or run-time is
> likely to decrease habituation, and therefore, it is critical to identify which security
> decisions users should be asked to make."

It also cites Felt et al.'s follow-up decision procedure as concluding "the majority of Android
permissions can be automatically granted, but 16% … should be granted via runtime dialogs" —
i.e. the recommended answer to prompt fatigue is *fewer, better-placed prompts*, not more.

A recent consumer-research article restates the same framing ("habituation", the privacy
paradox) in the current literature **[P]**: <https://onlinelibrary.wiley.com/doi/10.1111/joca.70044>.

### (d) URLs

<https://www.usenix.org/conference/usenixsecurity15/technical-sessions/presentation/wijesekera> ·
<https://ar5iv.labs.arxiv.org/html/1504.03747> ·
<http://cups.cs.cmu.edu/soups/2012/proceedings/a3_Felt.pdf> ·
<http://www.cyblog.cylab.cmu.edu/2012/07/cylabs-soups-continues-its-ongoing.html> ·
<https://developer.android.com/about/versions/11/privacy/permissions> **[P]** ·
<https://developer.apple.com/documentation/corelocation/cllocationmanager/requestwheninuseauthorization()> **[P]**

---

## 3. Flatpak and xdg-desktop-portal: asking for one file at a time

### (a) Mechanism and expressible scope

Flatpak's default sandbox is unusually tight **[F]**
<https://docs.flatpak.org/en/latest/sandbox-permissions.html>:

> "No access to any host files except the runtime, the app, `~/.var/app/$FLATPAK_ID`, and
> `$XDG_RUNTIME_DIR/app/$FLATPAK_ID`. Only the latter two being writable. **No access to the
> network.** … Limited syscalls. … Limited access to the session D-Bus instance."

Three mechanisms extend it, and they are different in kind:

1. **Static permissions** (`--filesystem=…`, `--socket=…`, `--device=…`, `--share=network`), set in
   the manifest's `finish-args` or by the user. The finest filesystem grant is a path (with `:ro`,
   `:create`), or a named XDG directory; and the docs warn: "Note, that **these permissions are
   completely static and variable expansion or substitution** (for example in `--filesystem` or
   `--env`) **is not possible**." **[F]**
2. **Portals** — the runtime-negotiated path **[F]**: "In many cases, portals use a system
   component to implicitly ask the user for permission before granting access to a particular
   resource. For example, in the case of opening a file, **the user's selection of a file using
   the file chooser dialog is interpreted as implicitly granting the application access to
   whatever file is chosen.**" The app calls `org.freedesktop.portal.FileChooser.OpenFile`, the
   backend shows a dialog, and the answer comes back as a `file://` URI
   (<https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.FileChooser.html>).
   Crucially, **"Files added to the Documents portal by this portal stay accessible across
   sessions"** — the grant is durable unless the portal entry is transient.
3. **The document portal** — the concrete "one file at a time" authority **[F]**
   <https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.Documents.html>:
   "Exported files will be made accessible to the application via a **fuse filesystem that gets
   mounted at `/run/user/$UID/doc/`**. The filesystem gets mounted both outside and inside the
   sandbox, but **the view inside the sandbox is restricted to just those files that the
   application is allowed to access.**" Files are added by passing an **open file descriptor**
   ("to prove that the caller has access to the file"), get a `doc_id`, and per-app permissions
   (`read`, `write`, `grant-permissions`, `delete`) are granted and revoked individually; the mode
   bits in the FUSE view reflect them.
4. **`flatpak override`** — the persistent, per-app or global adjustment **[F]**
   <https://man7.org/linux/man-pages/man1/flatpak-override.1.html>: "By default the application
   gets access to the resources it requested when it is started. But the user can override it on a
   particular instance by specifying extra arguments to `flatpak run`, or every time by using
   `flatpak override`." Overrides are plain text: "The application overrides are saved in text
   files residing in `$XDG_DATA_HOME/flatpak/overrides`"; `--nofilesystem` revokes, and
   `--nofilesystem=host:reset` drops everything inherited from the manifest.

### (b) What it cannot express

- **Per-host network control. Flatpak's network granularity is a boolean** (`--share=network` /
  `--unshare=network`). Worse, granting it is not just "the network" **[F, footnote 2 of the
  sandbox page]**:

  > "Giving network access also grants access to all host services listening on abstract Unix
  > sockets (due to how network namespaces work), and these have no permission checks. This
  > unfortunately affects e.g. the X server and the session bus which listens to abstract sockets
  > by default."

- **A grant broader than the portal entry leaks into the wider path**: `--nofilesystem=home` "does
  not prevent access to a more narrowly-scoped `--filesystem`" such as `xdg-config/MyApp`. Denies
  do not compose as a strict subtraction over narrower allows (the opposite of Codex's `deny`
  precedence in `sandbox.md` §3.3).
- **Reserved paths cannot be granted at all**: `--filesystem` has "no effect" for `/app, /bin,
  /dev, /etc, /lib, /lib32, /lib64, /proc, /run/flatpak, /run/host, /sbin, /usr`.
- **Portals require an app that asks.** A CLI tool that just calls `open("/etc/hosts")` gets
  `ENOENT`; there is no user-space hook that can intercept it after the fact. The mechanism only
  works for software written to use it (or a toolkit that implements portals for it).
- **Static permissions are not variable-expandable**, so a grant cannot be computed per task at
  launch time; the dynamic half is the portal's, and it is per object, one dialogue at a time.

### (c) Quotes worth reusing

- "the user's selection of a file using the file chooser dialog is **interpreted as implicitly
  granting** the application access to whatever file is chosen"
- "Export files … via a fuse filesystem … the view inside the sandbox is restricted to just those
  files that the application is allowed to access"
- "these permissions are completely static and variable expansion or substitution … is not
  possible"

### (d) URLs

<https://docs.flatpak.org/en/latest/sandbox-permissions.html> ·
<https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.FileChooser.html> ·
<https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.Documents.html> ·
<https://man7.org/linux/man-pages/man1/flatpak-override.1.html> ·
<https://man7.org/linux/man-pages/man1/flatpak-document-export.1.html>

---

## 4. Linux: Landlock and seccomp

### (a) Landlock — mechanism and expressible scope

**[F]** <https://docs.kernel.org/userspace-api/landlock.html> ·
<https://docs.kernel.org/security/landlock.html> · <https://landlock.io/>

- A stackable, unprivileged LSM. "The goal of Landlock is to enable restriction of ambient rights
  (e.g. global filesystem or network access) for a set of processes. … Landlock empowers any
  process, including unprivileged ones, to securely restrict themselves."
- Two rule types today: **filesystem rules** (object = a file hierarchy; rights EXECUTE,
  READ_FILE, READ_DIR, WRITE_FILE, TRUNCATE, IOCTL_DEV, RESOLVE_UNIX, REMOVE_*, MAKE_*, REFER) and
  **network rules**. On network: "**Network rules (since ABI v4 for TCP and v10 for UDP)**. For
  these rules, **the object is a TCP or UDP port**, and the related actions are defined with
  network access rights." The rights are `LANDLOCK_ACCESS_NET_BIND_TCP`,
  `_CONNECT_TCP`, `_BIND_UDP`, `_CONNECT_SEND_UDP`.
- Scope flags (ABI v6): `LANDLOCK_SCOPE_ABSTRACT_UNIX_SOCKET`, `LANDLOCK_SCOPE_SIGNAL` — IPC
  confinement to the domain, with **no allow-list exceptions**: "IPC scoping does not support
  exceptions via `landlock_add_rule(2)`. If an operation is scoped within a domain, no rules can
  be added to allow access to resources or processes outside of the scope."
- Compatibility model: the ruleset must declare `handled_access_fs` / `handled_access_net`
  explicitly, and the kernel's ABI version is read at runtime so userspace can apply "a best-effort
  security approach" and mask off rights the running kernel lacks. **This means a Landlock
  sandbox's strength varies with the kernel** — an ABI 1 kernel has filesystem rights only.
- Requires `no_new_privs` for unprivileged processes, and the docs warn that leaving it unset even
  when permitted is "risky … sandboxed processes could still execute set-user-ID, set-group-ID or
  file-capability binaries … making them potential confused deputies."

### The network correction (important)

The claim in `docs/sandbox.md` §3.4/§4 that "Landlock cannot express network policy" is **no
longer true**, and the distinction matters **[F]**: Landlock *can* restrict network **by port**
(TCP since ABI 4, UDP since ABI 10). What it cannot do is restrict by **address or host** — no
rule type in the ABI takes an address; the rule key is "a raw value (e.g. a TCP port)"
(`LANDLOCK_KEY_NET_PORT`). So Landlock can say "outbound TCP to port 443", and cannot say "only
`api.github.com`". The original network-support patch discussion is port-only throughout
**[F]** <https://kernsec.org/pipermail/linux-security-module-archive/2023-March/036673.html> — the
struct field is a port (`__u64 port`, checked against `U16_MAX`), and the only mention of an IP
address is about byte order on the wire, not about a rule.

### Can a ruleset be narrowed or extended after creation?

Precise answer, from the kernel docs **[F]**:

- A **ruleset** is a userspace object you can keep adding rules to (`landlock_add_rule`) until you
  enforce it.
- An enforced **domain is immutable**: "Once a thread is landlocked, **there is no way to remove
  its security policy; only adding more restrictions is allowed**. These threads are now in a new
  Landlock domain, which is a merger of their parent one (if any) with the new ruleset."
- Additional layers narrow: "Each time a thread enforces a ruleset on itself, it updates its
  Landlock domain with a new layer of policy. … A sandboxed thread can then safely add more
  constraints to itself with a new enforced ruleset." Access is the **intersection** of all layers.
- Two asymmetries: rights are bound at `open()` time and travel with the file descriptor ("It is
  possible that a process has multiple open file descriptors referring to the same file, but
  Landlock enforces different things when operating with these file descriptors", including across
  processes via fd passing); and files opened **before** sandboxing are unaffected.

### (b) What Landlock cannot express — the documented list

- Per-address or per-host network policy (above).
- Several file-related actions, verbatim **[F]**: "It is currently not possible to restrict some
  file-related actions accessible through these syscall families: `chdir(2)`, `stat(2)`,
  `flock(2)`, `chmod(2)`, `chown(2)`, `setxattr(2)`, `utime(2)`, `fcntl(2)`, `access(2)`." So
  metadata, ownership and existence checks are outside the model.
- `LANDLOCK_ACCESS_FS_REFER` "is the only access right which is denied by default by any ruleset,
  even if the right is not specified as handled".
- OverlayFS: "A policy restricting an OverlayFS layer will not restrict the resulted merged
  hierarchy, and vice versa."
- Truncate/`creat` subtleties: `creat()` on an existing file needs the truncate right; `O_TRUNC`
  works without `WRITE_FILE`; `fallocate(FALLOC_FL_COLLAPSE_RANGE)` can shorten a file without the
  truncate right.
- No syscall-argument filtering — by design: "A Landlock rule shall be focused on access control on
  kernel objects instead of syscall filtering (i.e. syscall arguments), which is the purpose of
  seccomp-bpf."

### seccomp, and what people do instead

- seccomp-BPF filters **syscalls and their scalar arguments**; it can block `socket()`/`connect()`
  families but cannot resolve a hostname or compare a `sockaddr`'s IP — that is Landlock's own
  division of labour (`docs.kernel.org/userspace-api/seccomp_filter.html`). Claude Code's sandbox
  uses it for exactly the narrow job of blocking Unix-socket creation
  (`@anthropic-ai/sandbox-runtime`), not for egress policy **[F]**
  <https://code.claude.com/docs/en/sandboxing>.
- The portable answer to "which host" is **a proxy outside the sandbox with an allowlist**:
  - Claude Code **[F]**: "Network access is controlled through a proxy server running outside the
    sandbox"; "the built-in proxy enforces the allowlist based on the requested hostname and, by
    default, does not terminate or inspect TLS traffic" — "code running inside the sandbox can
    potentially use domain fronting or similar techniques to reach hosts outside the allowlist."
  - Codex's Linux pipeline **[F]** <https://openai.github.io/codex/architecture/sandboxing/>:
    `--unshare-net` plus "an internal TCP→UDS→TCP bridge", "Only configured hosts reachable",
    "seccomp blocks new `AF_UNIX`/`socketpair` creation for user command".
  - Field example of the Landlock-plus-proxy pattern: a project's own security note records that
    "Linux (Landlock): Landlock drops the `:443` rule and allows only the proxy port"
    **[P]** <https://github.com/navikt/cplt/blob/main/SECURITY.md>.
- Windows-style packet filtering on Linux is `nftables`/eBPF, which needs privilege the agent
  generally does not (and should not) have.

### (d) URLs

<https://docs.kernel.org/userspace-api/landlock.html> ·
<https://docs.kernel.org/security/landlock.html> ·
<https://landlock.io/> ·
<https://kernsec.org/pipermail/linux-security-module-archive/2023-March/036673.html> ·
<https://docs.kernel.org/userspace-api/seccomp_filter.html> ·
<https://code.claude.com/docs/en/sandboxing>

---

## 5. Windows: restricted tokens, AppContainer capabilities, WFP

### (a) Mechanism and expressible scope

**Restricted tokens** **[F]**
<https://learn.microsoft.com/en-us/windows/win32/api/securitybaseapi/nf-securitybaseapi-createrestrictedtoken>

`CreateRestrictedToken(ExistingTokenHandle, Flags, DisableSidCount, SidsToDisable,
DeletePrivilegeCount, PrivilegesToDelete, RestrictedSidCount, SidsToRestrict, NewTokenHandle)`.
Three narrowing knobs, and the shape of the access check is the whole story:

> "Specify a list of restricting SIDs, which the system uses when it checks the token's access to a
> securable object. **The system performs two access checks: one using the token's enabled SIDs, and
> another using the list of restricting SIDs. Access is granted only if both access checks allow
> the requested access rights.**"

Flags: `DISABLE_MAX_PRIVILEGE` (0x1), `SANDBOX_INERT` (0x2), `LUA_TOKEN` (0x4),
**`WRITE_RESTRICTED` (0x8)** — "The new token contains restricting SIDs that are considered **only
when evaluating write access**." Also `SidsToDisable` gives deny-only SIDs: "The system uses a
deny-only SID to deny access to a securable object. The absence of a deny-only SID does not allow
access."

The write-restricted variant, explained by Microsoft's own service-hardening blog **[F]**
<https://learn.microsoft.com/en-us/archive/blogs/voy/write-restricted-token>:

> "Writes are only possible by virtue of the service SID, the logon SID, Everyone SID, or
> write-restricted SID, and not the service account nor its groups. Reads are unaffected."
>
> "By default, the write-restricted service looses write access to a lot of resources it would
> normally have access to by virtue of its account and groups. **Write access must be explicitly
> granted to the service SID, the logon SID, the write-restricted class, or Everyone to be
> possible.**"
>
> "It is a measure of good citizenship: your code runs write-restricted and its impact on the
> system in case of an exploit is mitigated. **Is it expensive? You bet it is. You have to
> determine all write accesses your code will need and make sure you explicitly grant that access
> to your service.** Are there tools to do so? We don't know, please comment…"

Two further Microsoft warnings on the same page **[F]**: restricted applications "should run …
on desktops other than the default desktop … to prevent an attack by a restricted application,
using `SendMessage` or `PostMessage`, to unrestricted applications"; and when using an existing
restricted token, the restricting-SID list of the new token is the **intersection** with the old
one.

**AppContainer / LPAC** **[F]**
<https://learn.microsoft.com/en-us/windows/win32/secauthz/appcontainer-isolation> and
<https://learn.microsoft.com/en-us/windows/win32/secauthz/implementing-an-appcontainer>

- "When an application runs as an AppContainer, its access token includes a unique application
  package identity (Package SID) and one or more capability SIDs."
- Access is the **intersection** of the user/group DACL and the AppContainer DACL: "if the User has
  full access, but the AppContainer only has read access, the AppContainer can only be granted read
  access."
- AppContainers run at **Low integrity**; device, file, registry, network, process, window and
  credential access are "blocked by default" and granted by capability. Read/write of specific
  persistent files and registry keys is a DACL grant.
- Capabilities are names resolved to SIDs (`DeriveCapabilitySidsFromName`): `internetClient`,
  `privateNetworkClientServer`, `internetClientServer`, `location`, `webcam`, …; LPAC adds
  `registryRead`, `lpacCom`, etc.
- Launching is a `STARTUPINFOEX` + `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES` dance (plus
  `PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY` for LPAC), with a profile directory and
  redirected `TMP`/`TEMP`/`LOCALAPPDATA`.

**WFP** is the address-level layer **[F]**
<https://learn.microsoft.com/en-us/windows/win32/fwp/windows-filtering-platform-start-page>: "The
WFP API allows developers to write code that interacts with the packet processing that takes place
at several layers in the networking stack … Network data can be filtered and also modified before
it reaches its destination." Windows Firewall itself is built on it. It is a driver-level API and
a separate product to write; Codex's Windows sandbox does not do per-host egress, and neither does
DSH's (see `docs/sandbox.md` §4.6/§4.8).

**The reference implementation to read** is Chromium's Windows sandbox design doc **[F]**
<https://raw.githubusercontent.com/chromium/chromium/main/docs/design/sandbox.md>. Its most
quotable lines:

> "At its core, the sandbox relies on the protection provided by four Windows mechanisms: A
> restricted token, The Windows *job* object, The Windows *desktop* object, Integrity levels."

> "**The restrictions are by design coarse** in that they affect all securable resources that the
> target can touch, but sometimes a more finely-grained resolution is needed." — the escape hatch
> is `AddRule` proxying a specific API call to the broker, of the form
> `AddRule(SUBSYS_FILES, FILES_ALLOW_READONLY, L"c:\\temp\\app_log\\d*.dmp")`, and "**Rules can only
> be added before each target process is spawned, and cannot be modified while a target is
> running**".

> On AppContainer's network capability: "The capability most interesting from a sandbox perspective
> is denying … access to the network, as it turns out **network checks are enforced if the token is
> a Low Box token and the `INTERNET_CLIENT` Capability is not present**."

> On LPAC and ACLs: "**all locations in the filesystem and registry that the LPAC process will
> access during its lifetime need to have the right ACLs on them.** … other files that the sandbox
> process needs access to including the binaries (e.g. chrome.exe, chrome.dll) and also any data
> files need ACLs to be laid down. This is typically done by the installer … if the LPAC sandbox is
> to be used in other environments then **these filesystem permissions need to be manually laid
> down using `icacls`, the installer, or a similar tool.**"

> On the bootstrapping hazard: the target "start[s] executing with a token that is very close to
> the token the regular user processes have", keeps a privileged initial token as the main
> thread's impersonation token, and "**Make sure any sensitive OS handles obtained with the initial
> token are closed before calling `LowerToken()`. Any leaked handle can be abused by malware to
> escape the sandbox.**"

> On residual holes: "By design, the sandbox token cannot protect the non-securable resources such
> as: Mounted FAT or FAT32 volumes: The security descriptor on them is effectively null … TCP/IP
> … Some unlabelled objects"; and "Under Windows, there is no practical way to prevent code in the
> sandbox from calling a system service."

### (b) What cannot be expressed

1. **Per-host network policy.** AppContainer network capability is a single SID
   (`internetClient`); it is present or absent. Windows offers no "only `api.github.com`" in the
   token/capability model — that is WFP's job, at driver level. Chromium's doc makes the same
   point from the other direction: it adds the Low Box attribute purely to *deny* network, because
   denial is the only thing the capability check can express.
2. **Read scope on Windows without a full token/ACL scheme.** `WRITE_RESTRICTED` restricts only
   writes; "Reads are unaffected". Read confinement needs either a fully restricted token (with
   the desktop caveat) or a broker.
3. **Fine-grained, dynamic, per-call grants.** Chromium's rules cannot be modified while a target
   runs; AD ACLs are per-object and persistent; the mechanism's natural answer to "one file, once"
   is to keep a broker and duplicate a handle.
4. **A clean story for `$PATH` and binaries.** A restricted token that cannot read a directory
   cannot *execute* what is in it (this is the same failure Codex documented in its permission
   profiles — see `docs/sandbox.md` §3.3), so `:minimal` has to be discovered per machine.
5. **Everything a leaked handle or an inherited fd can do.** Landlock has the same property, but on
   Windows it is compounded by the two-token bootstrap and by any third-party DLL injected into the
   process ("anti-malware solutions … can create backdoors to other processes or to the file
   system itself").

### (c) "How hard is a correct ACL sandbox?" — the honest answer

The three quoted costs, together, are the answer: (i) "You have to determine all write accesses
your code will need" (Microsoft, on write-restricted services); (ii) "all locations in the
filesystem and registry that the LPAC process will access during its lifetime need to have the
right ACLs on them" (Chromium, on LPAC); (iii) denied-by-omission is silent and the residual
holes are documented rather than closed (null DACLs, FAT volumes, unlabelled objects, system
services). The parent's own reading of `@deepseek-ai/dsh`'s 2,063-line
`dsh-sandbox-windows-acl` runner is independent confirmation of the same bill of materials
(`docs/sandbox.md` §3.1).

### (d) URLs

<https://learn.microsoft.com/en-us/windows/win32/api/securitybaseapi/nf-securitybaseapi-createrestrictedtoken> ·
<https://learn.microsoft.com/en-us/archive/blogs/voy/write-restricted-token> ·
<https://learn.microsoft.com/en-us/windows/win32/secauthz/appcontainer-isolation> ·
<https://learn.microsoft.com/en-us/windows/win32/secauthz/implementing-an-appcontainer> ·
<https://learn.microsoft.com/en-us/windows/win32/fwp/windows-filtering-platform-start-page> ·
<https://raw.githubusercontent.com/chromium/chromium/main/docs/design/sandbox.md>

---

## 6. macOS Seatbelt / `sandbox-exec`

### (a) Mechanism: deprecated, undocumented, still the only game in town

The man page says it in the NAME line **[F]** <https://keith.github.io/xcode-man-pages/sandbox-exec.1.html>:

> `sandbox-exec` — execute within a sandbox (DEPRECATED)
>
> "The `sandbox-exec` command is **DEPRECATED**. Developers who wish to sandbox an app should
> instead adopt the App Sandbox feature described in the App Sandbox Design Guide."

Usage: `sandbox-exec [-f profile-file] [-n profile-name] [-p profile-string] [-D key=value ...]
command [arguments ...]`. The profile language (SBPL) is **not documented by Apple at all** — the
man page documents only the flags, and the replacement it points at (App Sandbox, entitlements) is
for signed `.app` bundles, not for confining an arbitrary CLI process tree.

`sandbox(7)` gives the enforcement model in two sentences **[F]**
<https://keith.github.io/xcode-man-pages/sandbox.7.html>:

> "New processes inherit the `sandbox` of their parent. **Restrictions are generally enforced upon
> acquisition of operating system resources only.** For example, if file system writes are
> restricted, an application will not be able to `open(2)` a file for writing. However, **if the
> application already has a file descriptor opened for writing, it may use that file descriptor
> regardless of restrictions.**"
>
> "It is not a replacement for other operating system access controls."

### Who still uses it, and how

- **Codex** **[F]** <https://openai.github.io/codex/architecture/sandboxing/>: "Codex uses Apple's
  Seatbelt sandbox via `/usr/bin/sandbox-exec`"; "Seatbelt profile generation at runtime"; profiles
  assembled from an embedded base (`seatbelt_base_policy.sbpl`) plus permission clauses
  (`macos_preferences = "readonly"`, `macos_automation = [...]` → Apple Events send only to listed
  bundle IDs, `macos_accessibility`, `macos_calendar` → specific mach lookups). Test surface:
  `codex sandbox macos [--log-denials] [COMMAND]...`.
- **Claude Code** **[F]** <https://code.claude.com/docs/en/sandboxing>: "On macOS, there is nothing
  to install: sandboxing uses the built-in Seatbelt framework." It is the only platform where
  Claude Code needs no extra package (Linux/WSL2 need bubblewrap + socat); native Windows is not
  supported. Its docs also list real SBPL-shaped failures: "Go-based CLIs fail TLS verification on
  macOS: tools such as `gh`, `gcloud`, and `terraform` may fail TLS verification under Seatbelt";
  "`open`, `osascript`, or browser-based auth flows fail with error `-600` on macOS: the sandbox
  blocks Apple Events by default."
- **Chromium** has a Seatbelt design document **[P]**
  <https://chromium.googlesource.com/chromium/src/+/main/sandbox/mac/seatbelt_sandbox_design.md>
  (two fetches failed; cited as a pointer, not quoted). The macOS implementation is referenced from
  the same Windows design doc.
- **Agent runtimes** in the wild also generate SBPL profiles directly; one published research note
  on "macOS sandbox-exec capabilities and limitations" **[P]**
  <https://github.com/dredozubov/hazmat/blob/master/docs/research/macos-sandboxing-internals.md>.

### (b) What it cannot express / the honest limits

1. **It is not a documented interface.** SBPL has never had a public reference; profiles are
   written by reverse engineering and copied between projects. That alone makes it a fragile
   foundation for a product.
2. **Denials are opaque.** Seatbelt does not tell the process *which rule* denied an operation —
   the syscall just fails with a generic errno, and the reason (if any) appears in the system log,
   not in the failure. The evidence is what the tools had to build around it:
   - Codex ships a `--log-denials` flag "Log all denied operations" **[F]** — something you need
     only if the default failure text does not say.
   - Claude Code documents that it constructs the explanation itself **[F]**: "Claude Code reports
     sandbox violations in the blocked command's result, **naming the path or host the sandbox
     denied**, so Claude sees what the sandbox blocked."
   - Codex issue #23505 **[F]** <https://github.com/openai/codex/issues/23505> is the failure mode
     in the field: the missing clause is `(allow mach-lookup (global-name
     "com.apple.sysmond.system"))`, but the user sees "`sysmon request failed with error: sysmond
     service not found` / `pgrep: Cannot get process list`". Nothing in that message says
     "sandbox". The same issue records that the profile "is not written to disk, so end users
     cannot patch it without forking and rebuilding" — another real cost of generated SBPL.
   - Sibling issue: `danger-full-access` needed to get MLX/Metal IOKit access working
     **[P]** <https://github.com/openai/codex/issues/17644>.
3. **No per-object dynamic grants.** There is no equivalent of the Flatpak document portal or the
   Chromium broker: the process-wide profile is fixed at exec time, so "may I read this one file"
   has no native answer. Tools either widen the profile for the whole session or run the command
   unsandboxed (`excludedCommands`, `dangerouslyDisableSandbox`).
4. **Network rules exist in SBPL, but at the level of the profile, with no portable spelling.**
   Codex's docs claim "Network access control via `SandboxPolicy`" on macOS **[F]**, and
   `docs/sandbox.md` §5 asserts "macOS: network rules in SBPL can" express egress. I could not
   verify the actual SBPL operations from a primary source in this pass — treat the *syntax* as
   **unverified**; the *capability* is attested by two independent projects.
5. **Everything opened before the sandbox** stays usable, per `sandbox(7)` above.

### (d) URLs

<https://keith.github.io/xcode-man-pages/sandbox-exec.1.html> ·
<https://keith.github.io/xcode-man-pages/sandbox.7.html> ·
<https://openai.github.io/codex/architecture/sandboxing/> ·
<https://code.claude.com/docs/en/sandboxing> ·
<https://github.com/openai/codex/issues/23505> ·
<https://github.com/anthropics/sandbox-runtime>

---

## 7. Design writing: ambient authority, least authority, and agents

### Classic roots

- **Saltzer & Schroeder, "The Protection of Information in Computer Systems" (1975)** **[F]**
  <https://web.mit.edu/Saltzer/www/publications/protection/>. Two glossary entries are the whole
  argument in miniature:
  > **Capability**: "In a computer system, an unforgeable ticket, which when presented can be taken
  > as incontestable proof that the presenter is authorized to have access to the object named in
  > the ticket."
  >
  > **Prescript**: "A rule that must be followed before access to an object is permitted, thereby
  > introducing an opportunity for human judgment about the need for access, so that abuse of the
  > access is discouraged."

  The second is the honest name for an approval prompt: a prescript is a *cost*, and the paper's
  design principles (least privilege, fail-safe defaults, complete mediation, separation of
  privilege) are the reason a system wants as few prescripts as possible.
- **Object-capability security / ambient authority** **[P]**: Miller, Yee & Shapiro, "Capability
  Myths Demolished" — <https://papers.agoric.com/papers/capability-myths-demolished/full-text/>
  (also <https://srl.cs.jhu.edu/pubs/SRL2003-02.pdf>). The frame to import: authority should be a
  thing you *hold and pass* (a reference, a handle, an fd), not a property of *who you are* (your
  uid, your token, your process). Landlock's own framing is the same phrase from the kernel side:
  "restriction of **ambient rights**" (<https://docs.kernel.org/userspace-api/landlock.html>).
  Flatpak's document portal and Deno's permission broker are capability-shaped; a uid-based
  sandbox is not.

### Agent-specific

- **Simon Willison, "The lethal trifecta for AI agents" (2025-06-16)** **[F]**
  <https://simonwillison.net/2025/Jun/16/the-lethal-trifecta/>:
  > "The **lethal trifecta** of capabilities is: **Access to your private data** … **Exposure to
  > untrusted content** … **The ability to externally communicate** in a way that could be used to
  > steal your data … If your agent combines these three features, an attacker can **easily trick
  > it** into accessing your private data and sending it to that attacker."
  >
  > "**Guardrails won't protect you** … we still don't know how to 100% reliably prevent this from
  > happening."

- **Meta, "Agents Rule of Two"** (summarized and quoted by Willison) **[F]**
  <https://simonwillison.net/2025/Nov/2/new-prompt-injection-papers/>:
  > "agents **must satisfy no more than two** of the following three properties within a session …
  > **[A]** An agent can process untrustworthy inputs; **[B]** An agent can have access to sensitive
  > systems or private data; **[C]** An agent can change state or communicate externally … If an
  > agent requires all three without starting a new session (i.e., with a fresh context window),
  > then the agent should not be permitted to operate autonomously."

  Willison's own caveat is worth keeping: the "untrusted inputs + change state" corner is not
  actually safe, which "undermines the simplicity of the 'Rule of Two' framing."
- **Beurer-Kellner et al., "Design Patterns for Securing LLM Agents against Prompt Injections"
  (arXiv:2506.08837)** **[F]** <https://arxiv.org/abs/2506.08837>:
  > "once an LLM agent has ingested untrusted input, it must be constrained so that it is
  > impossible for that input to trigger any consequential actions."
- **Debenedetti et al., CaMeL, "Defeating Prompt Injections by Design" (arXiv:2503.18813)** **[F]**
  <https://arxiv.org/abs/2503.18813> — the design-writing bridge from ocap to agents:
  > "CaMeL uses a notion of a **capability** to prevent the exfiltration of private data over
  > unauthorized data flows by **enforcing security policies when tools are called**."

  Reported result: 77% of AgentDojo tasks solved with provable security vs 84% undefended — the
  utility cost of the capability layer, which is the number a design doc wants.
- **Nasr et al., "The Attacker Moves Second" (arXiv:2510.09023)** **[F, via Willison]**: 12 published
  prompt-injection/jailbreak defenses bypassed at ">90% for most"; human red-teaming 100%. The
  reason to prefer architectural containment (capabilities, sandboxes) over classifiers.
- **Agent sandbox escapes in practice**: the closest thing to a citable catalogue is the issue
  trackers rather than a paper — e.g. Codex's Seatbelt gaps above, and the recurring
  "sandbox silently disabled on macOS 15+" class of bug **[P]**
  <https://github.com/astrid-runtime/astrid/issues/855>. Flag: I did not find a peer-reviewed
  "agent sandbox escape" paper in this pass.

### URLs

<https://web.mit.edu/Saltzer/www/publications/protection/> ·
<https://papers.agoric.com/papers/capability-myths-demolished/full-text/> ·
<https://simonwillison.net/2025/Jun/16/the-lethal-trifecta/> ·
<https://simonwillison.net/2025/Nov/2/new-prompt-injection-papers/> ·
<https://arxiv.org/abs/2506.08837> · <https://arxiv.org/abs/2503.18813> ·
<https://arxiv.org/abs/2510.09023>

---

## 8. Design lessons

### The two named tasks

**"A one-off cross-workspace task"** (write one artifact into a sibling checkout, or read one file
outside the workspace):

| Model | Verdict |
|---|---|
| Flatpak document portal / FileChooser | **Best.** The user picks the object; the app receives an fd-backed handle (`/run/user/$UID/doc/<id>/name`) with `read`/`write`/`delete`/`grant-permissions` bits, revocable per app, transient or persistent by choice (`persistent` flag; `--transient` on `flatpak document-export`). This is the only mechanism surveyed where the *unit of grant is the object*, and where a durable-vs-session choice is a parameter rather than a mode. |
| Deno path-scoped flags | **Good, coarse.** `--allow-write=/some/dir` is exactly the right shape, but it must be decided at launch and it applies to the whole process, so a one-off discovered mid-task cannot be added without a prompt (which is process-wide once answered, except for the `request()` call itself). |
| Landlock | **Good but monotonic and address-blind.** You can grant one extra directory as a new narrowing layer for the process — but only by pre-planning or by re-execing under a broker, because the domain can never be widened after enforcement. |
| Windows restricted token + ACL | **Worst of the fine-grained options.** The grant is a persistent ACL edit or a broker round-trip; the token cannot be widened for a running process, and Microsoft's own guidance is that enumerating the writes is the hard part. |
| The coarse ladder | **Worst.** The only rung that permits it is `danger-full-access`, for the rest of the session — the exact "the sandbox teaches people to turn it off" failure in `sandbox.md` §1.2. |

**"Download from one site"** (one host, no other egress):

| Model | Verdict |
|---|---|
| Deno | **Best.** `--allow-net=example.com:443` is a first-class, per-host, per-port grant at launch, and `--allow-net --deny-net=github.com` carves exceptions. |
| Flatpak | **Worst.** `--share=network` is a boolean, and it also exposes every host service on an abstract Unix socket. There is no per-host portal. |
| Landlock | **Cannot.** Port-only (`LANDLOCK_ACCESS_NET_CONNECT_TCP` on a port). `:443` is expressible; `api.github.com` is not. |
| Windows AppContainer | **Cannot.** `internetClient` is one SID; per-host means WFP (driver-level) or a proxy. |
| macOS Seatbelt | **Probably can** (network operations exist in SBPL) but undocumented and unverified here; both Codex and Claude Code put a proxy in front anyway, and Claude Code's proxy is explicitly defeatable by domain fronting because it trusts the client-supplied hostname. |
| Proxy + allowlist | **The portable answer**, and the only one that also gives logging, credential injection/masking and TLS policy — at the cost of trusting a hostname and of the agent's traffic having to route through it. |

### Cross-cutting lessons

1. **The unit of grant should be an object, not a mode, and not a whole category.** The only
   mechanisms surveyed that grant "this one file" are fd/handle-based: the Flatpak document portal,
   Chromium's broker duplicating handles, and (in the abstract) a capability. Everything else
   grants a *class* (a path prefix, a host, a capability name, a port).
2. **Per-host network policy is not a kernel primitive anywhere.** Linux: port-only (Landlock) or
   syscall-level (seccomp). Windows: one capability SID, or WFP. Flatpak: a boolean. Android/iOS:
   no network permission scope at all beyond on/off (Android's `INTERNET` is install-time/binary).
   macOS is the possible exception and it is undocumented. If flint wants "reach this host", the
   mechanism is a proxy it controls, with the honest caveat Claude Code writes down.
3. **Monotonic sandboxes need a broker.** A Landlock domain, a restricted token, and an SBPL
   profile cannot be widened after the fact. Two consequences: the interesting authority has to be
   decided *before* the process starts, and any "ask the user at runtime" design must put the
   decision **outside** the confined process — a broker (Deno's `DENO_PERMISSION_BROKER_PATH`), a
   privileged parent proxying calls (Chromium), or a portal daemon (xdg-desktop-portal). This is
   the single most reusable architectural fact in the survey, and it maps directly onto flint's
   `decide()` living in the parent while `bash` children are confined by the OS.
4. **Prompt fatigue is measured, and it argues for fewer prompts with more context, not more
   prompts.** Felt 2012 (17% attention, 3% comprehension) and Wijesekera 2015 (habituation;
   80% wanted to block at least one request; a third of requests felt invasive) together say that
   an approval dialog on the happy path is worse than useless — it trains the answer. The design
   response the literature supports and the platforms converged on: ask only where the alternative
   is denial (`sandbox.md` §4.4), make the ask name the missing thing, give it a *duration*
   (once / turn / always), and make the durable answer land in a hand-editable file.
5. **Denial text is part of the mechanism.** Claude Code had to construct "naming the path or host
   the sandbox denied"; Codex ships `--log-denials`; Seatbelt's raw failure is
   `sysmond service not found`; Windows says `Access is denied`. A capability model whose denials
   are indistinguishable from ordinary errors will be argued with, retried, and ultimately
   disabled. This is the strongest external argument for flint's §4.3 `Decision.reason` /
   `missing` design.
6. **Every model surveyed collapses at the subprocess boundary, and the collapse is documented,
   not hypothetical.** Deno: `--allow-run` ≡ `--allow-all` ("a subprocess … runs with its own
   permissions"). Landlock/Seatbelt: pre-existing fds and inherited domains. Windows: leaked
   handles, injected DLLs, "no practical way to prevent code in the sandbox from calling a system
   service". Flatpak: network access implies access to host abstract sockets. Design lesson: state
   the boundary explicitly, per host, in the prompt — which is what flint's §4.6 proposes.
7. **The escape hatch has to be a first-class, named path.** Deno's prompt, Codex's
   `dangerouslyDisableSandbox` + `acceptForSession`, Claude Code's `dangerouslyDisableSandbox` and
   `excludedCommands` — every shipping system provides one, and the ones that name it and log it
   are the ones whose users keep the sandbox on.
