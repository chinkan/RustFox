# ADR-0058: Telegram Cantonese STT + TTS (software path)

- **Status:** Accepted
- **Date:** 2026-11-02
- **Amended:** 2026-11-03 (#363 voice-mode prompt, platform-only STT/TTS)
- **Issue:** #349 (v1.0.9 Must); amended by #363 (v1.0.9 Must)
- **Product ACCEPT:** Grill Round 1 2026-11-02; Soft ACCEPT Soft/contract #363 2026-11-03

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
7. **Voice-mode prompt (#363):** After code STT succeeds, that turn is **voice-origin**. The agent appends a voice-mode instruction to the system prompt for that turn only (short, spoken, ready to say aloud; no long tool report). Product default copy, used when soul and user files are empty or contain CJK: 「你而家用語音回覆：答案要短、口語、適合直接講出嚟；唔好長篇工具報告。」 Latin-only soul/user text may use the English adaptation of the same instruction. The install-written English `SOUL.md` template is not a language choice and keeps the Cantonese sentence until that file or `USER.md` is customized. A failed note, including one that keeps a typed caption, is not voice-origin and does not get the prompt. The prompt is applied to the turn that starts from that voice note. A voice note steered into an already-running turn keeps the transcript text on the steer message; this amend does not also speak that in-progress typed turn.
8. **Platform-only STT/TTS (#363):** Transcription and synthesis stay in `asr` and `telegram_voice`. A normal voice-origin turn hands the model the transcript text plus the voice-mode instruction. It does not ask the model to shell out (`execute_command` or otherwise) or write code to run SenseVoice, whisper, ffmpeg, or a TTS binary. Code STT runs before the model. Code TTS runs after the text reply when `[voice].tts` is true.
9. **TTS only for voice-origin replies (#363):** `sendVoice` is the second copy of a voice-origin reply. Typed chat does not `sendVoice` even when `[voice].tts` is true. The text copy is still sent (Decision 4). Synthesis or `sendVoice` failure leaves that text in the chat.
10. **Not in #363:** Continuous duplex / PTT; ESP32 #353; raising `max_iterations`; default-on cloud STT/TTS; Portal wizard voice; a `spawn_agents` / peer conversational lane (follow-on).

## Consequences

- Amends ADR-0020 Decision 9 follow-up: replace “type instead” placeholder path with real local STT for Voice notes.
- #363 amends Decisions 2 and 4: the interactive turn is still ordinary text, and TTS is still dual and config-gated, but the voice-origin turn also gets the voice-mode prompt, and `sendVoice` is not applied to typed chat.
- Soft: fixtures + QA Telegram smoke (voice in / optional TTS out). #363 Soft is unit tests plus the Telegram injector with mocked STT/TTS (happy path and no self-STT/self-TTS). No tag without Kan ship GO.
- OpenRouter fallback must be opt-in and visible in config/docs (privacy).

## Rejected

- Routing ESP32 / Device WS mic through the Telegram voice download path.
- Default-on cloud STT or TTS.
- Blocking #351 on this ADR.
