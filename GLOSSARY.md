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
