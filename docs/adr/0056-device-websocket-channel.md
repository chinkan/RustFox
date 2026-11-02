# ADR 0056: Device WebSocket channel

## Status
Accepted (Product 2026-11-02; grill Round 1–2)

## Date
2026-11-02

## Context
RustFox tip has no WebSocket server. Non-Telegram ingress is Portal REST + SSE chat (ADR-0005 deferred WS for live push). MCP is outbound tools only. Kan wants ESP32-C6 ↔ RustFox long-lived duplex for voice→home control; Product grilled Device WS as a **device adapter** into the existing agent loop (not a peer Channel). Numbering assumes ADR-0055 (soul visible window) lands on main; rename if needed.

## Decision
1. Add opt-in **`[device_ws] enabled` (default `false`)**. Same process Axum as portal. Portal UI may be off while `device_ws` is on (headless fox-home + device).
2. When **either** `[portal]` or `[device_ws]` is enabled, share bind+port. Path: **`/api/device/v1/ws`**. If only `device_ws` is on: minimal Axum (API-only; SPA not required).
3. **Identity = device adapter.** Authenticated frames become a standard interactive inbound user message (existing interactive-lane / TurnGate). Same owner primary conversation; turn metadata may carry `device_id` / room. **Do not** invent a third peer Channel or turn-gate.
4. **Auth:** per-device token, distinct from portal login token. Accept `Authorization: Bearer <token>` **or** first-frame `{"type":"auth","token":"..."}`. Tokens issued by fox-home; Portal Settings → Devices (Create / Rotate / Revoke); plaintext shown once; store hash or SecretStore ref. Revoke = delete/rotate. No anonymous WS.
5. **Framing (v1.0.9 MVP):** JSON text frames only.
   - Client→server: `auth` (if unused header), `user_text` (required for turns), `ping`, optional `device_hello` (`device_id` / room / fw_version).
   - Server→client: `assistant_text`, `device_ack`, `pong`, optional `error`.
   - Out of scope: binary audio frames, partial token streaming.
6. **vs #349 TTS/STT:** text handoff. #349 = software Telegram (etc.) voice. ESP32 does on-device/edge STT and sends text; RustFox may later return TTS/PCM for the board — not the same capture pipeline.
7. **Network assumptions:** MVP = loopback / Tailscale / LAN trusted (same spirit as ADR-0006 / 0007). Docs must state bare public WS is not recommended. TLS via external reverse proxy; no built-in TLS in this ADR.

## Rejected
- Replacing portal chat SSE with WebSocket for browser chat.
- Reusing request→SSE as device duplex.
- Routing hardware mic through the #349 Telegram voice pipeline.

## Consequences
- New WS dependency (e.g. axum WebSocket / tungstenite).
- Config + Portal Devices UI + SecretStore (or hash store) for device tokens.
- Publish a stable contract (path, frames, auth) for the separate `rustfox-esp32-c6` client repo; **firmware does not live in this ADR body**.
