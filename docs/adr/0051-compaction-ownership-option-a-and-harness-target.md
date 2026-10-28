# ADR-0051: Compaction ownership (Option A) and target harness

- **Status:** Accepted (Product lock Kan 2026-10-28 — do not reopen)
- **Date:** 2026-10-28
- **Amends:** [ADR-0003](./0003-conversation-compaction-redesign.md) (cross-provider ownership and target UX; Q1–Q9 pipeline decisions in 0003 stay authoritative)
- **Related:** [ADR-0043](./0043-opencode-detect-and-peer.md) (continuity SoT — SQLite `messages[]` for all Active peers; Direction D Go peer), [ADR-0045](./0045-register-time-tool-name-dedupe-and-empty-progress-clarify.md) (cold-intro / W1 ∪ W2 / CONTINUITY_LIGHT — demotion sequencing only), OpenCode compaction docs (https://opencode.ai/v2/docs/compaction), Product research summary 2026-10-28 (OpenCode vs RustFox compaction — evidence URLs in §Sources; not duplicated here)
- **Issue / PR:** Refs #326 (P0 protected tail / mid-turn hard-cap), #327 (impl); harness parity and cold-intro removal are **later** impl issues (out of this docs PR)

## Context

RustFox compacts conversation history under [ADR-0003](./0003-conversation-compaction-redesign.md): per-turn trigger, running `[SUMMARY]` as system role, protected tail, USER.md flush, and shared prepare/hard-cap paths for **all** Active peers.

[ADR-0043](./0043-opencode-detect-and-peer.md) locks **Direction D**: Active OpenCode = **Go cloud OpenAI-compat** chat-completions; RustFox owns the tool loop and builds `messages[]` from **SQLite** (`ConversationManager`). Local `opencode serve` / `opencode run` are **not** the Active chat path. The OpenCode **agent harness** (separate product surface) has its own auto/manual compaction (`compaction.auto`, summary + recent tail); that harness does **not** run on today’s shipped Go peer path — there is no live double-compact on Active D.

OpenRouter, OpenAI, Anthropic, xAI, Ollama, and OpenCode Go all need a **single** client-side compact owner because providers do not summarize RustFox’s agent history on chat-completions. Product asked whether RustFox should skip compact when Active provider is OpenCode to avoid double compaction.

Separately, P0 [#326](https://github.com/chinkan/RustFox/issues/326) / [#327](https://github.com/chinkan/RustFox/pull/327) fixes `protected_tail_start` advancing past `last_user` so mid-turn hard-cap must not drop the current ask or produce a system-only LLM pack (LangSmith dig `d958e9dc`). That invariant is a **prerequisite** for trusting long harness-shaped sessions.

Anti cold-intro ([ADR-0043](./0043-opencode-detect-and-peer.md) §Continuity, [ADR-0045](./0045-register-time-tool-name-dedupe-and-empty-progress-clarify.md) W2++) is a **phrase-matcher arms race** against models that claim lost context after compaction or hard-cap mistakes. Product target is to **remove** cold-intro guards once compaction/harness behavior is verified — not to keep widening matchers indefinitely.

This document records Product locks. It does **not** implement Rust, tests, Portal/Telegram UX, OpenCode config, or a release tag.

## Decision — Option A: RustFox always owns compact

1. **Single owner:** RustFox **always** owns context compaction for **all** Active peers, including Active **OpenCode Go** (Direction D). Continuity SoT remains SQLite → `messages[]` + `[SUMMARY]` rows ([ADR-0043](./0043-opencode-detect-and-peer.md) continuity locks).

2. **No provider skip:** Do **not** add `skip compact if provider=opencode` (or any Active-provider exemption). OpenCode Go receives the same bounded pack RustFox sends to OpenRouter (system + running summary + protected tail + current turn).

3. **Harness vs Go peer:** OpenCode **harness** compaction exists in upstream docs (auto default, manual API, disable via `compaction.auto: false`). The **shipped Active path** does not use that harness session for transcript SoT; Go is a chat-completions peer only. **No live double-compact** on Active D today.

4. **Future local OpenCode harness:** If Product later ships **local OpenCode harness** (parked Direction C / serve+run under RustFox), operators **must disable** OpenCode `compaction.auto` (and must not dual-enable harness auto-compact **and** RustFox compact). RustFox compact stays on; SoT stays SQLite.

5. **Reject Option B/C for current architecture:** Do not skip RustFox compact to “trust OpenCode” on the Go peer (no harness compact to trust). Do not build dual SoT sync (Option C) unless Product re-opens harness-as-outer-agent with an explicit protocol — out of scope here.

## Decision — Target harness (Kan 2026-10-28)

These locks describe the **product target** for every LLM pack RustFox assembles (implementation follows ADR-0003 + follow-on PRs; wording aligns with OpenCode-style summary shape as a **pattern**, not as delegating SoT to OpenCode):

1. **Baseline pack:** Each LLM request = **system** + **history since last `/clear`** (including running summary injection per ADR-0003) + **current ask** (and in-loop tool traffic for the active exchange).

2. **On overflow:** Summarize older history into a running summary that preserves **status**, **current context**, **pending actions**, and **next step** (agent-resume friendly). Show **user-visible compacting** on Telegram/Portal where Product ships UX (same family as ADR-0003 user-facing status — follow-on impl). Continue with **system + compacted summary + recent protected history + active exchange**.

3. **Hard invariants (align #326 / #327):**
   - **`protected_tail_start` never advances past `last_user`.** Post-ask tool traffic stays in the protected active exchange.
   - **Mid-turn hard-cap must not drop the current ask** and must **never** send a **system-only** pack inside the agentic loop (LangSmith dig `d958e9dc` class).
   - Full history remains in SQLite; only the **derived LLM view** may mask/truncate non-protected traffic.

4. **OpenCode `context_window` / prepare:** Accurate provider windows for `opencode-go/*` ids so ADR-0003 **85%** trigger is correct remains an impl follow-up ([ADR-0043](./0043-opencode-detect-and-peer.md) parked note on OpenCode `context_window` / prepare compaction — **policy** is now Option A here; window catalog maintenance is engineering, not a second compact owner).

## Decision — Cold-intro demotion sequencing (Accepted; not in this PR)

1. **End state:** After target harness behavior is **verified** in production (overflow summarize + invariants above + user-visible compacting as scoped), **remove cold-intro entirely**: structural final-reply guard **W1 ∪ W2**, light **CONTINUITY_LIGHT** prompt injection, and related regen/clarify rewrite paths on the shared Active final-reply path ([ADR-0043](./0043-opencode-detect-and-peer.md), [ADR-0045](./0045-register-time-tool-name-dedupe-and-empty-progress-clarify.md)). Goal: stop phrase-list arms race; honest continuity comes from compaction + tail policy, not matcher patches.

2. **Explicit sequencing (do not reorder):**
   - **First:** P0 protected tail / mid-turn hard-cap — [#326](https://github.com/chinkan/RustFox/issues/326) / [#327](https://github.com/chinkan/RustFox/pull/327) (impl closes #326).
   - **Second:** Harness **target parity** (§Target harness) in RustFox compact/prepare/UX follow-ons.
   - **Third:** Cold-intro **removal** — separate issue/impl PR later; **not** part of this docs PR or #327.

3. **Until step 3 ships:** Existing anti cold-intro MUSTs in ADR-0043 / ADR-0045 remain in force. This ADR does **not** weaken or delete them early.

## Non-goals

- Implementing compact, prepare, Telegram “Compacting…”, or OpenCode config in this docs PR.
- Release tag, v1.0.8 Soft gate, or Telegram Soft for this ADR.
- Reopening ADR-0003 Q1–Q9 (turn-boundary compact, running summary, flush, defer-on-failure, etc.).
- Parked Direction C delegate/harness as Active default; per-conversation `x-opencode-session` persist; #194 / #202.

## Consequences

- Operators and implementers have one rule: **RustFox compacts; providers including OpenCode Go do not replace that responsibility on Active D.**
- Future harness integration must **disable** OpenCode auto-compact to avoid dual summarization and divergent SoT.
- Engineering priority for long sessions: land #326/#327 before marketing harness parity; only then plan cold-intro removal.
- ADR-0003 remains the implementation spec for the compact **pipeline**; ADR-0051 is the **ownership + target UX + demotion schedule** amend.

## Sources (evidence — not reproduced in full)

- OpenCode compaction: https://opencode.ai/v2/docs/compaction  
- OpenCode Go inference (routing header, not transcript SoT): https://opencode.ai/docs/go/  
- RustFox: [ADR-0003](./0003-conversation-compaction-redesign.md), [ADR-0043](./0043-opencode-detect-and-peer.md), [#326](https://github.com/chinkan/RustFox/issues/326), [#327](https://github.com/chinkan/RustFox/pull/327)  
- Product research summary 2026-10-28 (OpenCode vs RustFox compaction) — internal evidence; executive table and Option A recommendation only cited here
