# RustFox

A local personal assistant: one sandbox, Telegram, and an optional portal.

## Language

**Plan**:
A named checklist of ordered steps in one sandbox. Each step has a status and a note.
_Avoid_: default plan, task list

**Active plan**:
The plan that view and update use when no title is given.
_Avoid_: latest file, default

**Fallback chain**:
Ordered list of fully-qualified `provider/model` entries tried after the primary chat completion fails (config `[fallback] chain`). Walks on almost any primary failure; hard-excludes only 401/403 and pre-HTTP missing-key/config errors (ADR-0019).
_Avoid_: OpenRouter native models routing, silent backup

**OpenRouter error envelope**:
HTTP response (often status 400) whose body nests upstream failures (`previous_errors`, `metadata`, documented `error_type`). Parsed for logs and typed inner 429/5xx; does not alone gate fallback after ADR-0019.
_Avoid_: string-matching "rate limit" in free text

**Mid-run queue**:
Per bot+user FIFO of messages that arrive while a turn is running (cap 10, in-memory). Each item is injected into the running turn, starts the next turn, or is refused with a reply when full. Never silently dropped (ADR-0020).
_Avoid_: injection map, pending injections

**Steer**:
Injecting a queued user message into the running turn between loop iterations (never mid-tool), so the final reply covers it. Default `/mode`; scheduled runs are never steered (ADR-0020).
_Avoid_: interrupt, preempt

**Quoted reply**:
The prior Telegram message a user is answering via `reply_to_message` (text/caption, or a media/unavailable placeholder). Injected into **this turn's** Active `messages[]` as part of one composed user message (ADR-0048). Not Continuity history rewrite, not an emoji Reaction, not Steer.
_Avoid_: Continuity, Reaction, Steer, Mid-run queue

**Inbound reply-to**:
Telegram-only (v1) ingress path: when an update carries `reply_to_message`, compose the locked `[Reply to {role} msg {id}]:` prefix with the quoted body and the user's new text into one user message, persist that composed text in existing SQLite history, and keep the quote on `QueuedInput` if mid-run (ADR-0048 + ADR-0020). Distinct from Continuity (#279/#282/#285), Reaction (follow-up), and Steer.
_Avoid_: Continuity, Reaction, Steer, Mid-run queue

**Schedule timezone**:
The IANA zone (e.g. `Asia/Hong_Kong`) a scheduled task's cron or naive one-shot time is read in. Stored per task; new tasks get the effective default (config `[general] timezone`, else system local, else UTC). Pre-ADR recurring rows keep `UTC` and pre-ADR one-shot rows get the system local zone, so no fire time changes; the user can change them later. Shown in the portal Tasks page (ADR-0022).
_Avoid_: server time, UTC offset (`+08:00`)


**Scheduled run timeout**:
Wall-clock bound on one scheduled agent call (`scheduler.run_timeout_secs`, default 900). On expiry the run is cancelled, marked failed, human-gated once (ADR-0013), and the job runner plus TurnGate are released (ADR-0024).
_Avoid_: unbounded schedule, forever cron

**Zombie running row**:
A `scheduled_task_runs` row left `status=running` after the process died or a run was abandoned. Cleared to `failed` on startup and by the stale-run watchdog (ADR-0024).
_Avoid_: stuck run, orphan status

**Built-in MCP**:
An MCP connection that ships with RustFox: endpoint fixed in code, turned on after setup by one tap (portal, or Telegram for Exa), one connection for the whole instance, credential (if any) in SecretStore. Exa may be offered as an optional skippable one-tap (keyless + consent) in the web setup wizard (ADR-0042); that is not a full MCP form. Google is unchanged and is not asked in the wizard. Credential still SecretStore / Settings. Today: Google (#119) and Exa web search (ADR-0023 / ADR-0042).
_Avoid_: preset MCP, default MCP server

**Keyless Exa**:
Exa web search used with no API key: free, rate-limited by Exa (HTTP 429 when exceeded). The default when Exa is enabled. A user's own key, stored in SecretStore and sent only as the `x-api-key` header, adds quota (ADR-0023).
_Avoid_: free tier key, shared key

**Web search stub**:
Placeholder tool named `web_search`, registered only while Exa is off. A call returns "web search is not enabled"; in Telegram it may offer an "Enable web search" button at most once per conversation. Removed when Exa is on (ADR-0023).
_Avoid_: fake search, dummy tool

**Sandbox mode**:
`[sandbox] mode = "sandbox"`. `execute_command` runs under an OS sandbox confined to the workspace (Linux bubblewrap, macOS Seatbelt); unavailable on Windows in Phase 1. Fails closed if the sandbox cannot start (ADR-0025).
_Avoid_: safe mode (UI copy only), silent fallback

**Trusted mode**:
`[sandbox] mode = "trusted"`, the default when `mode` is missing. `execute_command` starts in the workspace but is not confined. File tools stay confined in every mode (ADR-0025).
_Avoid_: unsafe mode, no sandbox

**Tool cap**:
Rule that a caller-supplied `tools` list on `invoke_agent` / `spawn_agents` is intersected with the parent's tools and the agent's declared tools. Empty result is an error; never escalates (ADR-0025).
_Avoid_: tool override, tool grant

**How to use**:
Docs sidebar group for end-user / self-hoster how-tos: Setup · Config · Telegram · Portal · Update (ADR-0049). Getting started stays separate and links in; not an operator GUIDE dump.
_Avoid_: GUIDE, Usage (MVP slug), operator runbook

**Features** (docs):
One capability-catalog page (what / limits + links): tools · MCP · schedule · memory (ADR-0049). Not the daily Telegram how-to.
_Avoid_: Telegram how-to, feature dump, GUIDE

**Providers** (docs):
One page for choosing a provider, credentials, and Active·peer: OpenRouter · OpenCode · xAI · Ollama (ADR-0049). Not a Config knob dump.
_Avoid_: Config page, provider dump, settings reference
