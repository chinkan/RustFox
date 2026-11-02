# ADR 0057: SwitchBot Open API connector

## Status
Accepted (Product 2026-11-02; grill Round 1–2; Kan: Portal Settings enable)

## Date
2026-11-02

## Context
Kan wants RustFox to control home appliances via SwitchBot, with end-to-end speak → device → RustFox → SwitchBot. SwitchBot Open API v1.1 uses `Authorization` (token) plus HMAC-SHA256 `sign` over `token + t + nonce` with a **secret**; host `https://api.switch-bot.com`; ~10 000 calls/day. Kan locked enablement via **Portal Settings** (MCP/credential style), not setup wizard or CLI required prompts. Numbering assumes ADR-0055 lands; rename if needed.

## Decision
1. **Builtin connector** (not MCP-required for v1.0.9): register `switchbot_*` tools on the ToolRegistry.
2. **Secrets:** vault names `switchbot.api_token` and `switchbot.api_secret`. Config holds only `secret:` refs + `enabled`. Portal Settings: Enable + Token + Secret (masked); PATCH via whitelist (ADR-0006 style). Env override is advanced-only.
3. **HTTP:** every request builds v1.1 HMAC headers; base URL `https://api.switch-bot.com`.
4. **Must tools (v1.0.9):**
   - `switchbot_list_devices` → `GET /v1.1/devices` (optional cache)
   - `switchbot_get_status` → `GET /v1.1/devices/{deviceId}/status`
   - `switchbot_command` → `POST /v1.1/devices/{deviceId}/commands` body `{command, parameter, commandType}`
5. **Should (non-blocking):** `switchbot_list_scenes` + `switchbot_execute_scene`; Park if capacity is short.
6. **Out of scope v1.0.9:** BLE-only devices; per-SKU dedicated tools; built-in rate dashboard (still surface API errors; respect daily limit).
7. **Failure UX:** tool error strings → model Final in plain language; device channel may also emit `device_ack` (ADR-0056).

## Rejected
- Setup wizard / CLI required prompts for the key.
- Storing token without secret (HMAC impossible).
- MCP-only as the v1.0.9 Must path.

## Consequences
- Settings UI + two SecretStore keys; docs for obtaining Developer token/secret from the SwitchBot app.
- Unauthorized / rate-limit failures must be diagnosable in tool output.
