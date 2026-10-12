# ADR-0025: Sandbox confinement, phase 1 (file tools, execute_command, tool caps)

- **Status:** Accepted (decisions locked 2026-10-12)
- **Date:** 2026-10-12
- **Issue:** #186
- **Related:** #173, #185 (subagent sandbox tools fix)
- **Ships in:** v1.0.5 (Phase 1)
- **Out of scope:** Phase 2, OS sandbox on Windows (follow-up ADR)

## Context

RustFox gives the model a workspace, `sandbox.allowed_directory`
(`SandboxConfig` in `src/config.rs`; defaults to `<home>/workspace`). Today the
confinement is weaker than the name suggests:

- **File tools.** `read_file`, `write_file`, `list_files` and `send_file` in
  `src/builtin_tools.rs` call `validate_sandbox_path` in `src/tools.rs`. It
  canonicalizes existing paths (or the parent for new paths) and checks
  `starts_with` against the canonical sandbox. Gaps:
  - A **dangling symlink** reports `exists() == false`, so only the parent is
    checked and the later write follows the link to any target.
  - The check and the open are separate syscalls (**TOCTOU**): a symlink can be
    swapped in between.
  - The open itself has no no-follow semantics; Windows reparse points
    (junctions) are not rejected explicitly.
- **execute_command.** `src/command_tool.rs` (~L115) only sets
  `.current_dir(&self.sandbox_dir)`; `src/main.rs` (~L988) passes
  `config.sandbox.allowed_directory` into `CommandTool::new`. The child process
  can read and write anywhere the user can. "Confined to the workspace" is a
  prompt instruction (`config.example.toml` system prompt), not enforcement.
- **Caller-supplied tools.** `invoke_agent` and `spawn_agents` accept a `tools`
  argument (`src/agent.rs`, `tools_override` ~L1734, `run_subagent` ~L1825).
  In ad-hoc mode it replaces `subagents.default_tools`; for a named agent it
  replaces the agent's declared `tools` (~L1939). `effective_subagent_tools`
  (~L2535) only adds `read_skill_file` / `read_agent_file`. Nothing caps the
  override against the caller's own tools, so a restricted parent can grant a
  child more than it has.
- **Setup.** New installs write `config.toml` via the setup wizard
  (`src/setup/wizard.rs`: `format_config` ~L1314, `persist_wizard_toml` ~L699,
  web and CLI paths). The portal edits config via `src/portal/settings.rs`.
- **Docker.** The repository ships no `Dockerfile` today; Docker is discussed in
  docs (e.g. ADR-0022 §9). "Official Docker image" below applies to whatever
  image the project publishes.

## Decision

### 1. Threat model (Q1)

Protect against **model mistakes and prompt injection** (web pages, files, MCP
responses). The **owner is trusted** and may relax the policy (trusted mode).
Not in scope: a malicious local owner, kernel exploits, MCP servers (they run
with their own configured permissions).

### 2. File tools: resolve, then open without following (Q2)

Applies to every path-taking file tool (`read_file`, `write_file`,
`list_files`, `send_file`, and any future one), on **all OSes**, with **no
switch** (independent of `sandbox.mode`).

1. Canonicalize `allowed_directory` once per check.
2. **Existing path:** canonicalize fully (follow symlinks); the result must be
   inside the canonical sandbox.
3. **New path:** canonicalize the **parent** directory, then append the file
   name; the parent must be inside the sandbox.
4. **Dangling symlinks are never followed.** Detect with `symlink_metadata`
   (not `exists()`); a final component that is a symlink whose target does not
   exist is rejected.
5. **Open with no-follow semantics:** Unix `O_NOFOLLOW` on the final component;
   new files with `create_new` (`O_CREAT|O_EXCL`). Windows: open with
   `FILE_FLAG_OPEN_REPARSE_POINT` and reject any reparse point (symlinks and
   **junctions**).
6. **TOCTOU:** where the OS allows, open relative to an already opened sandbox
   directory handle (`openat`-style, no-follow per component). Where this is
   not possible, it is a **documented residual risk** and does not block
   Phase 1.

### 3. `sandbox.mode` (Q3/Q4)

New key `[sandbox] mode = "sandbox" | "trusted"`.

| Mode | execute_command |
|------|-----------------|
| `trusted` | Current behaviour: starts in `allowed_directory`, not confined. |
| `sandbox` | Runs under an OS sandbox confined to `allowed_directory`. Linux: bubblewrap (`bwrap`). macOS: Seatbelt (`sandbox-exec`). Windows (Phase 1): no OS sandbox, so `execute_command` is **unavailable** and returns the Windows error below. |

**Sandbox filesystem view (Linux bwrap; macOS Seatbelt profile equivalent):**
read-only system directories needed to run commands (`/usr`, `/bin`, `/sbin`,
`/lib*`, `/etc`, `/opt`); `allowed_directory` read-write; private `/tmp`,
`/dev`, `/proc`; nothing else (in particular not the user's home directory).
So `/etc/hostname` is **readable** and not writable; `~/` outside the workspace
is not visible.

File-tool rules (§2) are identical in both modes.

**Defaults:**

- `mode` missing (upgrade, hand-written config) ⇒ **`trusted`**. No behaviour
  change on upgrade.
- Setup wizard, new install:
  - **Windows** ⇒ writes `trusted`; portal shows a clear note that safe mode
    for commands is not yet available on Windows.
  - **Linux / macOS** ⇒ wizard **probes once** by actually launching
    `bwrap` / `sandbox-exec` with a trivial command. Success ⇒ writes
    `sandbox`. Failure ⇒ writes `trusted`, and the message depends on the
    cause (§3a):
    - **bwrap not installed** ⇒ install helper (**Install for me**) or the
      one-line install command.
    - **bwrap present but blocked** (Ubuntu 23.10+ AppArmor
      `kernel.apparmor_restrict_unprivileged_userns=1` or other user-namespace
      restriction) ⇒ the "blocked by system settings" message, with **no**
      install advice.
- Official Docker image ⇒ **`trusted`** (the container already isolates).
- Portal shows the current mode and lets the owner switch.

### 3a. Linux: helping the owner get to sandbox mode

- **bwrap missing:** the wizard shows an **Install for me** button. It installs
  bwrap through `pkexec` (the OS-native password prompt). RustFox **never**
  collects, sends through the portal, or stores the sudo password.
- Supported package managers: `apt`, `dnf`, `pacman`. Any other ⇒ treated as
  "cannot install".
- No GUI session (headless/SSH) or no `pkexec` ⇒ no button; show one copyable
  install command (e.g. `sudo apt install bubblewrap`).
- After install, re-run the probe. Only if the sandbox starts, write `sandbox`.
  If install fails or the probe still fails, write `trusted` and show the
  one-line command.
- **Blocked by system settings:** if bwrap is present but the probe fails due to
  AppArmor (Ubuntu 23.10+ `kernel.apparmor_restrict_unprivileged_userns`; e.g.
  user-namespace setup or `--unshare-net` loopback `RTM_NEWADDR: Operation not
  permitted`, see
  [convira-sandbox#2](https://github.com/Convira/convira-sandbox/issues/2)),
  tell the user the computer's system settings block safe mode. Do **not** tell
  them to install bwrap again.
- **No re-setup needed:** the portal has a one-click **Switch to safe mode**
  that re-probes before switching; it only writes `sandbox` on success.
- macOS needs no install (Seatbelt is built in). Docker defaults to `trusted`
  and shows no button.

### 4. Same policy for main loop and subagents (Q5)

Main loop, `invoke_agent`, `spawn_agents`, scheduled runs and bot peers all use
the same file-tool rules and the same `sandbox.mode`.

### 5. Tool cap for caller-supplied `tools` (Q6)

A `tools` argument on `invoke_agent` / `spawn_agents` is allowed but capped:

```
effective = requested ∩ parent_tools ∩ declared_tools   (declared_tools only if the agent declares any)
```

- Ad-hoc spawn with no declared list: cap = parent's tools.
- No `tools` argument: today's resolution, still capped by parent's tools.
- `read_skill_file` / `read_agent_file` are still added by
  `effective_subagent_tools`.
- **Empty intersection ⇒ error** (never run silently with zero tools).
- **No escalation**, ever, at any nesting depth.

### 6. Fail closed (Q7)

If `mode = "sandbox"` but the sandbox cannot start at runtime (Docker blocking
namespaces/seccomp, bwrap removed later, Seatbelt failure), `execute_command`
returns the "failed to start" error. It **never** silently falls back to
trusted.

### 7. Canonical error copy

These English strings are the **canonical copy**; code and tests match them
exactly (`{path}` is the user-supplied path; `{command}` is the one-line install command for the detected package manager).

| Case | Message |
|------|---------|
| Windows + `sandbox` | `Running commands is not available in safe mode on Windows yet. You can switch to trusted mode in the portal.` |
| Sandbox failed to start | `Running commands is not available in safe mode on this computer right now. You can switch to trusted mode in the portal. If you run RustFox in Docker, allow sandboxing in your Docker settings.` |
| Empty tool intersection | `No tools are available for this agent with the tools you asked for, so it was not started.` |
| Path outside sandbox | `The path {path} is outside your workspace folder, so it cannot be used.` |
| Install button | `Install for me` |
| Installing / password hint | `Installing the safety tool. Your computer may ask for your password; RustFox never sees it.` |
| Install failed + command | `We could not install the safety tool. You can install it yourself by running: {command}` |
| Blocked by system settings | `This computer's system settings block safe mode, so RustFox will run in trusted mode.` |
| Switched to safe mode | `Safe mode is on. Commands can now only use your workspace folder.` |

## Rejected alternatives

- **Lock every OS, including Windows, in Phase 1.** No mature unprivileged
  Windows sandbox fits the schedule (restricted token + ACL / MXC need their
  own design). Deferred to Phase 2.
- **Warn-only.** Prompt injection does not read warnings; no protection.
- **Silent downgrade to trusted when the sandbox fails.** Owner believes they
  are protected when they are not. Fail closed instead.
- **Landlock-only on Linux.** Kernel-version dependent, does not cover older
  distros, and gives no mount view; bwrap is widely packaged and testable.
  May be added later as defence in depth.
- **Disallow `tools` override entirely.** Breaks legitimate narrowing
  (giving a child fewer tools). Capping keeps the use case without escalation.

## Consequences

**Pros**

- Symlink, dangling-link and junction escapes through file tools are closed on
  every OS.
- On Linux/macOS new installs where the sandbox works, shell commands cannot
  touch files outside the workspace.
- Subagents can never get more tools than their caller.
- Upgrades keep working unchanged (`trusted` default).

**Cons**

- Upgraded installs stay unconfined for commands until the owner switches.
- Windows has no command confinement in Phase 1.
- Ubuntu 23.10+ default AppArmor policy blocks bwrap; many users land on
  `trusted` unless they change the sysctl or install an AppArmor profile.
- Some commands that need paths outside the workspace (global caches,
  `~/.cargo`) will fail in sandbox mode.
- TOCTOU remains a residual risk where `openat`-style opens are unavailable.

## Tests

### File tools (all OSes, both modes)

1. Dangling symlink `ws/a -> /nonexistent/x`: `write_file a` rejected, nothing
   created at the target.
2. Symlink to outside `ws/b -> /etc/passwd`: `read_file b` rejected with the
   canonical "outside" copy.
3. Windows junction `ws\j -> C:\Windows`: `list_files j` and `read_file j\win.ini`
   rejected.
4. TOCTOU (Unix): race a swap of `ws/c` between a regular file and a symlink to
   outside while calling `read_file` / `write_file` in a loop for **10,000
   swap attempts or 10 s, whichever comes first**; **zero** accesses outside
   the workspace allowed.
5. `../` and absolute outside paths (`../../etc/passwd`, `/etc/passwd`,
   `C:\Windows\win.ini`) rejected.
6. Write a new file via symlinked parent `ws/d -> /tmp`: `write_file d/new.txt`
   rejected.
7. Regular files and nested directories inside the workspace still work.
7a. `send_file`: absolute/`../` outside path rejected; `ws/e -> /etc/passwd`
    symlink rejected; both with the canonical "outside" copy.
7b. New-file creation uses `create_new`: if the target appears between the
    check and the create (pre-created file or symlink), the create fails and
    the existing file is never overwritten or followed.

### Config and wizard

8. Upgrade: existing config without `mode` ⇒ effective `trusted`.
9. Wizard-written config contains an explicit `mode`.
10. Hand-written config without `mode` ⇒ `trusted`.
11. Wizard probe success (Linux with working bwrap / macOS) ⇒ writes `sandbox`.
12. Wizard probe failure because **bwrap is not installed** ⇒ writes `trusted`
    and shows the install helper or one-line install command. (Blocked-by-
    system-settings case: test 30.)
13. Windows wizard ⇒ writes `trusted`, portal note shown.
14. Portal shows the mode and switching persists to `config.toml`.

### Runtime

15. Windows + `sandbox`: `execute_command` returns the canonical Windows copy.
16. `sandbox` with bwrap removed / namespaces blocked: `execute_command` returns
    the "failed to start" copy; no command runs unconfined.
17. `sandbox` on Linux/macOS: `cat /etc/hostname > ws/x` succeeds (system dirs
    are read-only visible); `touch /etc/x` fails; `ls ~` / `touch ~/outside`
    fails (home not visible); writes inside the workspace succeed.
18. Docker image default config ⇒ `trusted`. *Deferred: the repository has no
    Dockerfile yet.*

### Tool cap

19. Parent `[read_file]`, request `[read_file, execute_command]` ⇒ child gets
    `read_file` only.
20. Named agent declares `[read_file, write_file]`, request `[write_file,
    execute_command]`, parent has all ⇒ `write_file`.
21. Ad-hoc spawn, no declared list ⇒ capped to parent's tools.
22. Empty intersection ⇒ canonical "no tools" error, child not started.
23. Nested spawn cannot regain a tool the middle agent lacks.

### Main loop vs subagent

24. Cases 1–6 and 15–17 give identical results from the main loop and from a
    subagent.

### Linux install helper and portal switch

25. bwrap missing, GUI + pkexec + apt/dnf/pacman: button shown; install via
    pkexec; probe re-run; success ⇒ `sandbox` + "Safe mode is on" copy.
26. pkexec cancelled or install fails ⇒ `trusted` + "could not install" copy
    with the right command.
27. Install succeeds but probe still fails ⇒ `trusted`.
28. Unsupported package manager ⇒ no button; command shown; `trusted`.
29. Headless/SSH or no pkexec ⇒ no button; one copyable command.
30. bwrap present, AppArmor blocks userns (CI job (a)) ⇒ "system settings
    block safe mode" copy; no install button or install hint.
31. No password ever appears in portal requests, logs, config or SecretStore
    (inspect network traffic and files during test 25).
32. Portal "Switch to safe mode": probe fails ⇒ stays `trusted` with the
    matching copy; probe succeeds ⇒ writes `sandbox`.
33. macOS: no install button. Docker part (no button, `trusted`) *deferred:
    the repository has no Dockerfile yet.*

### Verification

**Linux CI (ubuntu-24.04), two jobs:**

- **(a) Default runner.** `kernel.apparmor_restrict_unprivileged_userns=1`
  blocks bwrap
  ([actions/runner-images#10015](https://github.com/actions/runner-images/issues/10015)).
  Verifies: wizard probe fails ⇒ writes `trusted` and shows the "blocked by
  system settings" message (no install hint; test 30); `sandbox` mode fails
  closed (test 16).
- **(b) Userns enabled.** Same job after
  `sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0`. First runs a
  bwrap smoke test (e.g. `bwrap --ro-bind / / true`) and **fails the job** if
  bwrap cannot start, to avoid false passes. Then verifies sandbox confinement
  (tests 11, 17).

**macOS:** a `macos-latest` CI integration job exercises `sandbox-exec`
(Seatbelt) confinement. macOS acceptance for v1.0.5 is CI + unit tests; no
manual real-device gate.

**Manual QA:**

- Debian without bwrap: probe ⇒ `trusted`. Install bwrap, re-run: ⇒ `sandbox`
  mode works.
- Windows (on the maintainer's Windows PC): new install defaults to
  `trusted`; choosing `sandbox` ⇒ plain error on `execute_command`.

**Install helper (tests 25, 26):** verified in v1.0.5 by unit tests with a fake
`pkexec`, asserting the resulting mode and the user-facing copy. Exit codes per
[pkexec(1)](https://www.freedesktop.org/software/polkit/docs/latest/pkexec.1.html):
`126` = user dismissed the auth dialog; `127` = not authorized, auth failed or
pkexec error; after successful auth pkexec returns the package manager's own
exit code. Five cases:
- `0` (installed) and re-probe succeeds ⇒ `sandbox` + "Safe mode is on" copy.
- `0` (installed) but re-probe is blocked by AppArmor / user namespaces ⇒
  `trusted` + the "blocked by system settings" copy, and **no** install command.
- `126` (cancelled), `127` (not authorized), other non-zero e.g. `100`
  (`apt-get` install failed) ⇒ all write `trusted` and show the one-line
  install command.

**Password handling (test 31):** verified by code review that the sudo password
never passes through the portal and is never stored in config, database or
logs (`pkexec` owns the prompt).

**Known untested after v1.0.5:** the real-desktop `pkexec` flow has not been run
manually. User reports about it are treated as bugs immediately.

CI workflow changes are implementation work and are not part of this docs
change.

## Phase 2 (out of scope)

Windows OS sandbox (restricted token + ACL; MXC on Windows 11 24H2+) in a
follow-up ADR. After that, the Windows new-install default may move to
`sandbox`.
