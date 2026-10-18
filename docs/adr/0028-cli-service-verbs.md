# ADR-0028: CLI verb subcommands for OS service management

- **Status:** Accepted
- **Date:** 2026-10-18
- **Target release:** v1.0.7
- **Depends on:** none (does not block #201 / v1.0.6)
- **Issue:** (CLI service verbs — Notion row; GitHub issue may follow)

## Context

Today (`main` at the time of this ADR):

- Bare `rustfox` (optionally with `--config`) starts the bot in the
  foreground.
- `rustfox --setup` / `rustfox --setup --cli` runs the setup wizard.
- `rustfox --service <install|remove|status|start|stop>` manages the OS
  service. There is no `restart` action.
- `--service` is **management only**. The installed systemd unit, launchd
  plist, and Windows service all execute `rustfox --config …` — they do
  not pass `--service`.
- Linux install uses `systemctl --user enable --now` (install and start).
  Windows `install-service.bat` ends with `sc start`. macOS plist sets
  `KeepAlive=true`; stop currently uses `launchctl stop`, which launchd
  may immediately restart under KeepAlive.
- Portal wizard, CLI wizard, and `install.sh` all teach
  `rustfox --service install`.
- In-process `restart_pending` (self-upgrade) is separate from OS service
  restart.

Kan asked for clear verb subcommands (`install` / `start` / `stop` /
`restart` / foreground) instead of the `--service` flag. Verb spellings
are not locked by Kan; Product Owner locks them from industry practice.

## Decision

1. **Bare `rustfox` stays foreground.** Existing service units and docs
   keep working. Adding `rustfox run` as an explicit alias with the same
   behaviour. Do not use `dev`.
2. **Management verbs:** `install`, `uninstall` (with `remove` as a
   deprecated alias), `status`, `start`, `stop`, `restart`.
3. **`install` writes the unit and starts the service** (same as today:
   enable+start / `sc start` / load). No `--no-start` flag in this
   change.
4. **Idempotency:** If the service is already in the requested state
   (`already installed` / `already running` / `already stopped`), exit 0
   with one clear line. If the service is not installed, `start`,
   `restart`, and `uninstall` exit non-zero and tell the user to run
   `install` first.
5. **`uninstall` always true-stops first**, then disables and removes the
   unit. If already stopped, continue with removal.
6. **`restart` uses the platform-native path**, and is documented as
   distinct from self-upgrade `restart_pending`:
   - Linux: `systemctl --user restart rustfox.service`
   - Windows: `sc stop` then `sc start` (or equivalent)
   - macOS: `launchctl kickstart -k` on the agent
7. **macOS `stop` must truly stop.** Do not treat `launchctl stop` alone
   as success under `KeepAlive`. Unload / disable KeepAlive so the agent
   stays down until `start` or `install`.
8. **Idempotent `install` overwrites** the unit / plist / bat from the
   current templates, then ensures the service is started, so upgrades
   pick up KeepAlive / stop fixes without a manual reinstall dance.
9. **`--service …` is deprecated in the same release:** it still works,
   prints a stderr hint to the new verbs, and is removed in the next
   minor. `remove` remains a deprecated alias of `uninstall` on the same
   schedule.
10. **Portal / CLI wizard / `install.sh` / user-facing error strings**
    switch to the new verbs in the same change. Portal does **not** gain
    Start / Stop / Restart / Status buttons here — only copy and the
    install path. Extra buttons are a follow-up.
11. **`--setup` is unchanged** in this change. `rustfox setup` is out of
    scope.
12. **Self-upgrade `restart_pending` is out of scope** except for
    documentation that it is not `rustfox restart`.

## Non-goals

- Changing how the service process is invoked (`rustfox --config …`).
- Portal service control UI beyond install copy.
- Renaming or replacing `--setup`.
- Blocking v1.0.6 / #201.

## Acceptance criteria

- `rustfox run` and bare `rustfox` both start the bot in the foreground
  with the same config resolution.
- `rustfox install|uninstall|status|start|stop|restart` work on Windows,
  Linux (systemd user), and macOS (launchd), matching the semantics
  above.
- On macOS, after `rustfox stop`, the agent does not come back until
  `start` / `install` (no KeepAlive false-stop).
- `rustfox --service <action>` still works once, with a stderr deprecation
  hint naming the new verb.
- Portal wizard, CLI wizard, and `install.sh` no longer teach
  `--service` as the primary command.
- CHANGELOG / release notes for the shipping release describe the new
  verbs and the deprecation.
- Automated tests cover argument parsing (new verbs, deprecated
  `--service` and `remove`) and, where practical, idempotent messaging
  without requiring a live OS service on every CI host.

## Consequences

- Users get a clearer CLI; old scripts keep working for one minor.
- macOS stop/restart behaviour becomes trustworthy for non-technical
  users.
- Re-running `install` after upgrade refreshes service definitions.
- Implementation is assigned to Rust Developer (CLI + service templates +
  docs strings). UI only if portal copy lives in UI-owned files; no new
  control buttons in this ADR.
