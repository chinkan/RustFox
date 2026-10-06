# ADR 0017: OpenRouter model pick allows a typed model id

## Status
Accepted

## Date
2026-10-05

## Context
Thin setup (#118) locked a short OpenRouter pick list and rejected any id outside `OPENROUTER_MODELS` (“do not grow into a typed model id”). That list goes stale; users need ids from https://openrouter.ai/models that are not on the list. Ollama stays list/library-based (unchanged). Product locked the reversal on 2026-10-05, after Bug B and OpenRouter SecretStore (ADRs 0015–0016).

## Decision
- Keep the short list as shortcuts. Add a typed model id path (“其他” / Other): web shows the list plus a persistent text field (picking a list entry fills the field; the user may edit). CLI adds a numbered Other option then `read_line` for the id.
- Validation: non-empty and must contain `/` (covers `openrouter/auto` and `:free` suffixes). Do **not** call OpenRouter’s models API during setup. The curated list is no longer a hard allowlist.
- Empty typed field / empty Other input is rejected — do not silently fall back to `moonshotai/kimi-k2.6`. Default remains that id only when the user picks it or leaves the list default selected without clearing.
- Expose a link to https://openrouter.ai/models in the wizard UI (open externally when a browser open is appropriate; on WSL/headless show the URL only — same spirit as ADR 0015).
- Ollama detect / library / pull path is unchanged.
- Remove or relax `openrouter_model_allowed` so typed ids that pass the `/` rule can be written to config.

## Consequences
Config may hold any `provider/model` string the user typed; a bad id fails later at the API, not at allowlist time. GUIDE should mention the list is a shortcut, not the full catalog. No tag required for this alone.
