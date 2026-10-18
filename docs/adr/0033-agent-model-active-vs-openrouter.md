# ADR-0033: Agent/subagent model vs Active vs `[openrouter].model`

- **Status:** Proposed
- **Date:** 2026-10-19
- **Target release:** v1.0.7+ (does not block v1.0.6)
- **Depends on:** ADR-0029 (fallback → tool-weak rewrite+omit; #203 / #218 / #220), ADR-0032 (primary Ollama + tools; banner Active / `current_model` adjacent to #217)
- **Issue:** #223

## Context

Kan live (2026-10-19): Telegram weather via agent `hko-weather` failed with the
user-facing “temporarily unavailable or out of quota” sentence after dual-PID
hygiene left a single daemon.

Journal facts (QA-corrected):

- `hko-weather` had **no** agent-level `model` / `model_override`.
- Invoke used global `[openrouter] model = deepseek/deepseek-v4.1-flash`, **not**
  the banner **Active** model (`ollama/qwen3.8:27b` / `current_model`).
- deepseek hit OpenRouter **402** (credits / `max_tokens`); fallback onto
  Ollama returned **500** `no user query found` (mapped to the fake-quota
  sentence). Whether the agent-invoke walk always runs ADR-0029 rewrite+omit
  on tip ≥ `01db8f6` (#220) is an impl verification item.

**Pre-impl gap (document today):** agents/subagents with **no** agent-level
model can still follow `[openrouter].model` and diverge from banner Active.
That mismatch caused Kan’s OR 402 on weather and is the behaviour this ADR
locks to fix in a later impl PR.

Product lock (PO, 2026-10-19 HKT) is authoritative for the Decision below.

## Decision

1. **Explicit agent/subagent `model` is sticky** when the operator set one.
   Do **not** silently rewrite their config. Do **not** forbid cloud overrides
   while Active is local Ollama (power-user specialist agents stay valid).

2. **Shipped / example agents** (e.g. `hko-weather`): **no** agent-level model
   by default — **inherit Active**. Ops may clear a stale override without
   waiting on this issue.

3. **No agent-level model → follow Active.** When Active ≠ `[openrouter]
   model`, agent/subagent invoke with **no** agent-level model must use
   **Active** (`current_model` / banner), **not** `[openrouter].model`.

4. **Override / primary failure → existing fallback chain.** Unchanged: on
   override (or resolved) provider failure (404/402/unavailable), walk the
   global fallback chain. No special “force Active only” branch beyond that
   chain.

5. **ADR-0029 on every agent/subagent fallback → tool-weak walk.** Same as
   main chat (#218 / #220): rewrite + omit-tools whenever tools are present
   (first-turn or mid-loop). If tip ≥ `01db8f6` still lacks the rewrite warn
   on the agent-invoke path, treat as a **code gap** and fix in the impl PR
   (amend note on ADR-0029).

6. **Release:** target v1.0.7+. Must not block v1.0.6. GitHub English only.
   Four gates on the **impl** PR; this ADR PR is docs-only.

## Current gap (pre-impl)

Until the impl PR lands, agents without an override may still resolve to
`[openrouter].model` and diverge from banner Active. Document that gap here;
do not claim the runtime already follows Decision §3.

## Non-goals

- Dual-PID / shared Telegram token hygiene (soft / out of scope for #223).
- Forbidding cloud agent overrides while Active is Ollama.
- Blocking v1.0.6 tagging (Kan-owned). No tag from this issue.
- Replacing or weakening ADR-0029 / ADR-0032 primary keep-tools shaping.
- Portal UI redesign.

## Acceptance criteria

- Docs state: explicit agent `model` is sticky; shipped examples inherit Active.
- Weather (or equivalent) with Active = Ollama does not hit OR 402 solely
  because a shipped example hardcodes a cloud model, **or** because invoke
  followed `[openrouter].model` instead of Active when no agent-level model
  was set.
- When Active ≠ `[openrouter].model`, agent/subagent invoke with no
  agent-level model uses Active (`current_model` / banner).
- Agent-invoke fallback → tool-weak Ollama never sends a tools/user-less
  payload that 500s with `no user query found`; rewrite warn appears when
  rewrite ran (ADR-0029).
- CHANGELOG entry under the shipping release (impl) / Unreleased note for
  this docs slice.

## Consequences

- Docs-only first (this ADR + ADR-0029 amendment). Tech lead assigns RustFox
  Rust Developer for impl after merge (`fix/223-agent-active-model` or
  similar). Leave #223 open for impl — this PR is a docs slice only.
- Banner Active / `current_model` (#217) becomes the source of truth for
  no-override agent invoke, matching operator expectation.
- Specialist agents with an explicit cloud `model` remain valid even when
  Active is local Ollama.
