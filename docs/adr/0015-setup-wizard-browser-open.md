# ADR 0015: Setup wizard opens the browser on WSL via Windows, skips headless

## Status
Accepted

## Date
2026-10-05

## Context
`--setup` starts the web wizard on `http://localhost:8719` and after 400ms always runs `xdg-open` then `open`, ignoring failures. On WSL that often opens a blank or useless browser window even though the same URL works when opened from Windows (WSL2 localhost forwarding). Headless or no-`DISPLAY` Linux has nothing useful to open. The URL is already printed. Product locked this in the RustFox room on 2026-10-05 (Bug B). Not a release tag.

## Decision
- Detect WSL (e.g. `/proc/version` contains Microsoft/WSL). On WSL, try `wslview <url>` if present, else `cmd.exe /c start <url>`, to open the Windows default browser. Do not use `xdg-open` as the primary path on WSL.
- On non-WSL Linux with no usable display (no `DISPLAY` / headless), do not auto-open.
- On normal Linux, macOS, and Windows, keep trying to open the local URL (existing `xdg-open` / `open` / equivalent).
- Always print `http://localhost:8719` (or the bound URL). If auto-open is skipped or fails, print one extra line: ask the user to open that URL manually. Do not spell out WSL/headless detection in the message.
- CLI thin setup does not open a browser; this ADR only covers the web wizard path.

## Consequences
WSL users get a working Windows browser when `wslview`/`cmd.exe` succeed. A failed Windows hand-off still leaves a clear manual URL. No tag for this change alone.
