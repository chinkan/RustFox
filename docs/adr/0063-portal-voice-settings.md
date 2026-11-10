# ADR-0063: Portal Voice Settings

- **Status:** Product lock (ACCEPT pending Soft tip Final)
- **Date:** 2026-11-09
- **Amended:** 2026-11-09 (#385) — English TTS is a v1.0.9 Must (`sendVoice` for spoken `yue` or `en`)
- **Milestone:** v1.0.9 (Must/Should). Kan lock 2026-11-09
- **Amends:** [ADR-0058](./0058-telegram-cantonese-stt-tts.md) — the setup wizard stays out of scope; Portal **Settings** voice is in scope
- **Related:** [ADR-0006](./0006-portal-token-auth-and-safe-settings.md) (settings whitelist), [Telegram Cantonese voice](../telegram-cantonese-voice.md)

## Context

ADR-0058 locked Telegram voice notes on the bot path: local SenseVoice STT, opt-in Cantonese TTS, and opt-in OpenRouter STT. It left Portal **wizard** voice out of scope. Operators still place the model files by hand. Missing files fail that note and do not stop the process. Kan locked a SwitchBot-style control on Portal **Settings** so the same path can be switched, given a language set, and can fetch the packs that are already wired. Device WS text and the ESP32 mic stay a different pipeline (ADR-0056, ADR-0058).

The same-day amend (#385) adds English speech on that Settings path. Spoken `yue` and spoken `en` both `sendVoice`. `zh`, `ja`, and `ko` stay text-only. The English weights are a local sherpa pack beside the Cantonese pack, not a cloud voice.

## Decision

1. **Surface.** A Portal **Settings** section only, same idea as SwitchBot enablement (ADR-0057). The setup wizard gains no voice step. That is the whole amend to ADR-0058 Decisions 6 and 10.

2. **`voice.enabled` is the master gate** for the Telegram voice pipeline, including STT.
   - `false`: no STT on an inbound voice note (local or OpenRouter) and no TTS / `sendVoice`. Typed chat is unchanged. A caption stays ordinary text and is not voice-origin. A caption-less note is not a silent drop: the bot sends a short clarify that voice is off (ADR-0058 Decision 2).
   - `true`: the existing local SenseVoice path in ADR-0058 and [Telegram Cantonese voice](../telegram-cantonese-voice.md) applies.
   - An absent key defaults to **true**, so a config that already follows ADR-0058 keeps transcribing. Fetching packs does not run at process start.

3. **Languages.** Multi-select from the SenseVoice set `{yue, zh, en, ja, ko}`. Persist the selection as `languages`. An empty list or any other id is rejected and not written. Duplicates collapse. Stored order is `yue`, `zh`, `en`, `ja`, `ko`.
   - One selected id → `--sense-voice-language=<that id>`.
   - Two or more → `--sense-voice-language=auto`.
   - Default selection when both `languages` and legacy `language` are absent: **yue + en**, so the flag is `auto`.
   - When `languages` is absent and legacy `language` is set, that single id is the selection. An install pinned to `language = "yue"` stays on `yue` until Settings saves a new selection.

4. **One STT pack.** The multi-lang pack already pinned for this engine: `sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2025-09-09` ([Telegram Cantonese voice](../telegram-cantonese-voice.md)). Choosing a language does not download a separate STT model.

5. **Auto-download on Settings save.** The directory is under the existing home (`RUSTFOX_HOME`, then `[general].home`, then `~/.rustfox`).
   - A save with `enabled: true` ensures the SenseVoice pack at `<home>/models/sensevoice` (fetch and unpack when `model.int8.onnx` or `model.onnx`, plus `tokens.txt`, is missing).
   - A save with `tts: true` ensures **both** TTS packs under that home:
     - Cantonese files already wired at `<home>/models/tts-cantonese`.
     - The English pack at `<home>/models/tts-english`. The pinned archive is `vits-piper-en_US-lessac-medium` from <https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/vits-piper-en_US-lessac-medium.tar.bz2>. Unpack it so that directory contains `model.onnx` and `tokens.txt`. The archive names the weights `en_US-lessac-medium.onnx` (inside `vits-piper-en_US-lessac-medium/`); symlink that file to `model.onnx`. The archive also ships `espeak-ng-data/`, which stays beside those files. Fetch again when `model.onnx` or `tokens.txt` is missing. Synthesis reuses the Cantonese VITS command `sherpa-onnx-offline-tts` (`--vits-model`, `--vits-tokens`, and `--vits-data-dir` when `espeak-ng-data/` is present).
   - A save with `enabled: false` does not start a download and does not delete packs.
   - A failed or still-running download does not fail the save (a bad language list still returns 400 and writes nothing). GET and the PATCH body include read-only `voiceDownload` with three pack keys: `sensevoice`, `ttsCantonese`, and `ttsEnglish`. Each value is a status `absent`, `downloading`, `ready`, or `error`, plus a short message. A missing or failed pack does not crash the process. The next note in that language stays text-only (Decision 6). The other spoken language still speaks when its pack is ready. SenseVoice failure stays the ADR-0058 fail-soft path (clarify, or the typed caption).

6. **Spoken reply.** `tts` stays a separate opt-in and defaults to **false**. `enabled` allows the pipeline; `tts` adds speech. `sendVoice` runs for a voice-origin reply (ADR-0058 Decision 9) when `enabled` and `tts` are true and the spoken language is **`yue` or `en`**. The text copy is still sent. The spoken language is that id when the CLI flag is `yue` or `en`, or when the flag is `auto` and SenseVoice reports that id (JSON `lang`, or a `<|yue|>` or `<|en|>` tag read before the tags are stripped). **`zh`, `ja`, and `ko` stay text-only.** An `auto` result other than `yue` or `en`, or `auto` with no reported language, stays text-only. A missing pack for the spoken language stays text-only for that language. English TTS is a v1.0.9 Must (the pack in Decision 5).

7. **OpenRouter STT** stays the ADR-0058 opt-in, default off. Turning `enabled` on does not set it. It is not on this Settings whitelist. It runs only when `enabled` is true and `openrouter_stt` is already true.

8. **Settings API (ADR-0006).** Extend the GET/PATCH whitelist with non-secret voice fields: `voiceEnabled` (bool), `voiceLanguages` (string array), `voiceTts` (bool), and read-only `voiceDownload`. `voiceDownload` carries `sensevoice`, `ttsCantonese`, and `ttsEnglish`, each `absent`, `downloading`, `ready`, or `error`, plus a message (Decision 5). No tokens and no raw TOML. Backup `config.toml` before the write, same as the other settings. Publish the new voice config onto the in-memory config the Telegram path reads on the next note (`agent.config.voice` today), with interior mutability the way SwitchBot `apply_patch` updates its live settings. These fields stay off the `restartRequired` list. The weights are not loaded at startup: each note runs `sherpa-onnx-offline` (and, when speaking, `sherpa-onnx-offline-tts`) against the files on disk, so a finished unpack needs no process restart. This ADR does not install those binaries or `ffmpeg`. A missing binary is status `error` and the same per-note fail-soft path, not an exit and not a restart.

9. **Out of scope.** Device WS and the ESP32 mic path. Portal wizard. Cloud TTS. OpenCC s2hk (its own Should, if it is ever locked). A per-language STT download.

## Consequences

- ADR-0058 Decisions 6 and 10 still keep voice out of the setup wizard. Settings is the in-scope surface. English TTS for spoken `en` is a v1.0.9 Must of this ADR.
- The default selection yue+en uses `auto`, so a new save speaks when the recognizer reports `yue` or `en`. `zh`, `ja`, and `ko` stay text-only. A missing pack for the reported language stays text-only for that language.
- Soft tip Final is still pending before a tip squash. This decision does not cut a release tag.

## Rejected

- Voice steps in the setup wizard.
- One STT download per language. The shipped pack already contains `yue`, `zh`, `en`, `ja`, and `ko`.
- Failing the Settings save, or exiting the process, when a pack is missing.
- Default `enabled = false`. That would turn off the shipped ADR-0058 path. This lock names both states and does not default the master off; `tts` and OpenRouter STT do.
