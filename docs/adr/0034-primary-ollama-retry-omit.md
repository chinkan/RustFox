# ADR-0034: Primary Ollama `no user query` → rewrite+omit retry once

- **Status:** Proposed
- **Date:** 2026-10-19
- **Target release:** v1.0.7+ (does not block v1.0.6)
- **Depends on:** ADR-0032 (primary Ollama keep-tools; #213 / #216), ADR-0029 (fallback → tool-weak rewrite+omit; #203 / #218 / #220)
- **Issue:** #226

## Context

Kan live (2026-10-19 ~02:18–02:20 HKT): after ops pointed `[openrouter].model`
at Active `ollama/qwen3.8:27b`, Telegram `/clear` → “Today weather” still failed
with the user-facing fake-quota sentence.

Journal facts (PO):

- Turn ran on **my-finance-agent / default** — not `hko-weather` (that routing
  question stays with QA; out of scope here).
- Large system prompt → drop duplicate tools → **primary Ollama 500**
  `no user query found` → fallback OpenRouter **402** → Ollama 500 again →
  fake quota.
- No `Shaping primary Ollama…(ADR-0032)` / ADR-0029 rewrite warn on that turn
  (path / filter question for QA).

**Gap:** ADR-0032 locks **first-pass keep-tools** on primary Ollama. When that
keep-tools attempt still returns `no user query found` (large tool-set /
prompt compaction), there is **no** documented recovery on the same primary
before the global fallback chain. Ops then hit OpenRouter credits / a second
Ollama 500 and dead-end on fake quota.

Product lock (PO on #226, 2026-10-19 HKT) is authoritative for the Decision
below. Separate from #223 / #225 — do not expand those issues.

## Decision

1. **ADR-0032 first-pass keep-tools unchanged.** Primary Ollama still prefers
   keeping tools on the first attempt so MCP tool bubbles can complete.

2. **Primary recovery after `no user query found`:** when primary (Active)
   Ollama returns `no user query found` / an equivalent userless-payload reject
   **after** the ADR-0032 keep-tools attempt (main chat loop, or any invoke
   that already kept tools per ADR-0032):
   - **Rewrite + omit tools** and **retry once** on the **same** primary model
     (same spirit as ADR-0029 omit, but triggered by this primary error — not
     only on fallback).
   - Exactly **one** such primary omit-retry; do not loop.

3. **If that retry still fails** → continue the **existing** global fallback
   chain unchanged. Tool-weak Ollama on that walk remains ADR-0029
   rewrite+omit.

4. **Journal WARN** when this primary rewrite+omit retry runs (so ops can see
   it; do not silently swallow).

5. **Release:** target v1.0.7+. Must not block v1.0.6. GitHub English only.
   Four gates on the **impl** PR; this ADR PR is docs-only.

## Non-goals

- Replacing or weakening ADR-0032 first-pass keep-tools on primary.
- Changing ADR-0029 fallback rewrite / omit-tools for chain walks onto
  tool-weak Ollama.
- Expanding #225 / #223 (agent Active resolution, mid-flight scope).
- Dual-PID / shared Telegram token hygiene.
- Blocking v1.0.6 tagging (Kan-owned). No tag from this issue.
- Portal UI redesign.
- Requiring operators to raise Ollama `num_ctx` as the primary fix.

## Acceptance criteria

- Docs state: after primary Ollama `no user query` (post keep-tools), rewrite
  + omit tools and retry **once** on the same primary; then existing fallback.
- Active=Ollama + large toolset does not dead-end on fake quota solely from
  one keep-tools 500 without the omit-retry then fallback path.
- Journal shows a clear WARN when the primary omit-retry runs.
- CHANGELOG entry under the shipping release (impl) / Unreleased note for
  this docs slice.

## Consequences

- Docs-only first (this ADR + short cross-refs on ADR-0032 / ADR-0029). Tech
  lead assigns RustFox Rust Developer for impl after merge
  (`fix/226-primary-ollama-retry-omit` or similar). Leave #226 open for impl —
  this PR is a docs slice only.
- Primary keep-tools remains the happy path; omit-retry is a **recovery**
  path after `no user query`, not a replacement for first-pass keep-tools.
- Fallback behaviour (ADR-0029) stays the second line of defence after the
  one primary omit-retry fails.
