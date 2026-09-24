# ADR-0010: Decision-layer interface for LLM routing/pruning

- **Status:** Proposed
- **Date:** 2026-09-19
- **Relates to:** ADR-0004 (embedded portal), ADR-0009 (HTTP retry policy)

## Context

RustFox increasingly needs *atomic judgments over its own state* that are too
cheap or too frequent for a frontier chat model:

1. **Model routing** — per inbound message: frontier model or local Ollama?
   Tool use needed, or a direct DB lookup? (OpenRouter 429s cluster at
   12:00–12:50 HKT precisely because everything shares one pool — ADR-0009
   treats the symptom, routing treats the cause.)
2. **Context pruning** — per tool call during compaction: still relevant?
   result still needed verbatim? (Complements SelfCompact-style timing logic
   with keep/drop scoring.)
3. **Batch scoring** — arXiv daily briefing candidate selection; inbox sweep
   classification.

The Jev family of "System One" models (TypeSafe AI, Sept 2026) fits this
shape: typed probabilistic decisions (Noul/Choice/Score) from a single
forward pass, no generation, ~$0.04/M input tokens. **NanoJev**
(github.com/chinyuka/NanoJev, MIT, 429★ in 3 days) replicates the architecture
on Qwen3-0.6B with open weights, dataset, and training code — self-hostable
on the RustFox VPS (RTX 5090 24GB).

## Decision

Reserve a **decision-layer seam** now, implement nothing yet:

```rust
pub trait DecisionLayer: Send + Sync {
    /// P(true) per question about `state` (one forward-pass batch).
    fn judge(&self, state: &serde_json::Value, questions: &[Query])
        -> BoxFuture<'_, Result<Vec<f32>>>;
}

/// MVP: always defers to a fixed policy (current behaviour).
pub struct NullDecisions;
```

- `LlmClient::pick_route()` and compaction's keep/drop step call this trait;
  with `NullDecisions` the behaviour is byte-identical to today.
- The first real implementation is a local Qwen3-0.6B+heads checkpoint
  trained on **RustFox runtime telemetry** (verified outcomes, tool-result
  re-request events, sweep labels) — free labels, zero data egress.
- Noul per-candidate questions are preferred over a single Choice question
  (NanoJev's probe evidence: Choice over-concentrates on stated-probability
  tasks; Noul tracks explicit probabilities ~0.5).

## Consequences

- Portal keeps one API surface (`/api/decision-health`) reserved for a
  future "Context Health" screen; not built in M1–M4.
- No external API dependency (privacy: transcripts stay on-host).
- Explicitly deferred until the Portal backend merges; tracked as a spike
  after Milestone 4.

## Alternatives rejected

- **Call hosted Jev API now** — leaks tool transcripts (vault contents,
  parsed statements) to a third party with no paper yet and no self-host;
  NanoJev removes the only reason to accept that.
- **Hand-tuned heuristic router** — no learning signal from telemetry;
  every new model/provider needs manual rule edits.
