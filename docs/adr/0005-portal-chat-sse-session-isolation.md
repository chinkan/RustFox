# ADR 0005: Portal Chat over SSE with Isolated `web` Sessions

## Status
Accepted

## Date
2026-09-19

## Context
The portal chat must call the real agent loop (`Agent::process_message`, which already accepts `tool_event_tx` + `stream_token_tx` channels) and stream answers to the browser. Options: WebSocket (the prototype's `useWebSocket.ts` assumed one) vs REST + Server-Sent Events. Portal answers are strictly request→stream-response (the browser never pushes mid-generation), and cancel uses an existing control path (`Agent::cancel_processing`).

## Decision
**REST for CRUD, SSE for streams.**

- `POST /api/chat` with a JSON body returns `text/event-stream`. The browser consumes it with `fetch()` + a `ReadableStream` reader (manual SSE frame parsing), **not** `EventSource` (GET-only).
- Events: `token` (incremental text), `tool` (`ToolEvent` name/status), `done` (final answer), `error`.
- **Session isolation**: portal chat uses `platform = "web"` and `user_id = <portal user>` (default `"web"`, from `[portal] user_name`). Telegram uses `platform = "telegram"` and numeric user IDs, so conversations never cross-contaminate, while both share one runtime, one system prompt, and one long-term memory store.
- Cancel: `POST /api/chat/cancel` → `Agent::register_cancel_token` / `cancel_processing("web")`.
- The prototype's `useWebSocket.ts` is replaced by `useChatStream.ts` (fetch/SSE); WebSocket stays a follow-up for live dashboard push only.

## Rationale
- SSE is native to Axum (`tokio-stream` + `Event`), survives HTTP/1.1 and reverse proxies, and reconnect semantics are unnecessary here — if the stream drops, the run continues in-process and the final answer is already persisted in the conversation.
- No heartbeat/reconnect/auth-frame layer to build = MVP delivered faster.
- Session isolation prevents a browser "new chat" from wiping a user's Telegram history, and keeps cancel tokens user-scoped per platform.

## Consequences
- `process_message` is invoked with `tool_ui_mode = Verbose` so tool events reach the UI; token streaming requires the agent's stream path to be active for portal requests.
- One concurrent portal chat per `user_id` is assumed (matches single-user deployment). Concurrent sends from the same web user reuse the same cancel slot — acceptable for MVP.
- Long generations hold one connection; no server-side timeout is added in MVP.
- History rendering uses `GET /api/chat/history` (`MemoryStore::load_messages` for the active web conversation), so page reload never loses the transcript.
