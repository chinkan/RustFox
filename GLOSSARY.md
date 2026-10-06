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

**Schedule timezone**:
The IANA zone (e.g. `Asia/Hong_Kong`) a scheduled task's cron or naive one-shot time is read in. Stored per task; new tasks get the effective default (config `[general] timezone`, else system local, else UTC). Pre-ADR rows keep `UTC` until the user changes them. Shown in the portal Tasks page (ADR-0022).
_Avoid_: server time, UTC offset (`+08:00`)
