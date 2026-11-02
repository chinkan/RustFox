# ADR-0058: Telegram Cantonese STT + TTS (software path)

- **Status:** Accepted
- **Date:** 2026-11-02
- **Issue:** #349 (v1.0.9 Must)
- **Product ACCEPT:** Grill Round 1 2026-11-02

## Context

RustFox tip ignores caption-less Telegram voice notes (empty text + empty attachments → silent no-op). ADR-0020 Decision 9 planned voice download + `transcribe_audio` with a v1 placeholder; local STT never shipped. Kan wants bidirectional **Cantonese** voice on the **Telegram bot** path for v1.0.9 (not Portal wizard). Hardware capture is separate (#351 Device WS text + #353 ESP32 path B).

## Decision

1. **Scope:** Software Telegram STT + TTS only. Does **not** use #351 / #353 capture pipelines. Distinct from Device WS JSON text frames (ADR-0056 Decision 6).
2. **STT first (Must slice):** Ingest Telegram **Voice** notes only (not general `Audio` unless Product widens). Download → local STT → inbound text shaped as:
   ```
   [Voice note]:
   {transcript}
   ```
   Feed TurnGate / interactive lane like ordinary user text. **Fix silent drop** of voice-only messages (no empty early-return without transcript or honest clarify).
3. **Local-first STT:** Prefer on-box local engine (**SenseVoice** *or* **whisper.cpp + Yue/Cantonese model** — Dev picks one primary; document in impl PR). **OpenRouter** (or remote) STT fallback only when config **explicitly enables** it (default off / no silent cloud).
4. **TTS (same ADR, config-gated):** Default **off**. When enabled: outbound `sendVoice` as OGG/Opus **plus** text (dual). No TTS-only replies that drop the text copy.
5. **Shared ASR library vs #353:** May share the **same local ASR library/crate** for #353 path B off-device ASR. Must **not** share Telegram Bot API download / voice-note ingress with ESP32.
6. **Out of scope:** Portal wizard voice; binary audio on Device WS; Xiaozhi cloud ASR/Opus; non-Cantonese localization packs unless free with the same stack; stealing Dev from #351.

## Consequences

- Amends ADR-0020 Decision 9 follow-up: replace “type instead” placeholder path with real local STT for Voice notes.
- Soft: fixtures + QA Telegram smoke (voice in / optional TTS out). Contract docs (CHANGELOG + module docs) with impl PR.
- OpenRouter fallback must be opt-in and visible in config/docs (privacy).

## Rejected

- Routing ESP32 / Device WS mic through the Telegram voice download path.
- Default-on cloud STT or TTS.
- Blocking #351 on this ADR.
