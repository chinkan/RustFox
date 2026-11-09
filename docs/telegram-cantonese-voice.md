# Telegram Cantonese voice notes

Accepted lock: [ADR-0058](adr/0058-telegram-cantonese-stt-tts.md). This page is the operator setup for that Must slice (#349).

Voice notes are Telegram `voice` messages only. Music and other `audio` files are left alone. Device WebSocket text and the ESP32 mic path do not use this download or this ingress.

## What the bot does

1. Download the voice note with the Bot API (`getFile`, then the file URL).
2. Transcribe it with local **SenseVoice** (`sherpa-onnx-offline`), language `yue`.
3. Feed the interactive TurnGate the same way as typed text, shaped exactly as:

```
[Voice note]:
{transcript}
```

4. Mark that turn voice-origin and add a voice-mode instruction to the model for this turn only. The default sentence (empty soul/user files, or any CJK in those files) is: 「你而家用語音回覆：答案要短、口語、適合直接講出嚟；唔好長篇工具報告。」 A Latin-only soul or user file uses the same instruction in English. The English `SOUL.md` created on first run is only a template, so the Cantonese sentence stays until you edit `SOUL.md` or `USER.md`. The model is given the transcript as text. It is not asked to transcribe or synthesize with a shell or by writing code. RustFox code already did STT and, when enabled, does TTS.

A typed caption is kept after a blank line. If the note has no caption and download or transcription fails, the bot sends a short clarify and asks you to type the message. It does not return without saying anything. A failed note that keeps a typed caption is ordinary typed text: no voice-mode prompt and no spoken reply.

OpenRouter speech-to-text does not run unless you set `openrouter_stt = true`. A missing local model with the flag left at the default never posts audio to the network.

TTS does not run unless you set `tts = true`, and then only for a voice-origin reply whose spoken language is Cantonese (`yue`) or English (`en`). `zh`, `ja`, `ko`, and an `auto` result that did not come back as `yue` or `en` stay text. Typed chat stays text even when `tts` is true. When a voice reply is spoken, the chat still gets the usual text message **and** a `sendVoice` OGG/Opus note. Synthesis or `sendVoice` failure leaves the text in the chat. `enabled = false` skips STT and TTS; a note with no caption says voice is off. Portal Settings can change this without a restart (ADR-0063). A missing model does not stop the process.

## Primary engine

| Piece | Choice |
|-------|--------|
| Engine id | `sensevoice` (the only implemented id) |
| Binary | `sherpa-onnx-offline` on `PATH`, or `stt_bin` |
| Language | `yue` (Cantonese). Also accepted: `auto`, `zh`, `en`, `ja`, `ko` |
| Model | `sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2025-09-09` |
| Files | `model.int8.onnx` (or `model.onnx`) and `tokens.txt` in `model_dir` |
| Audio | Voice notes are OGG/Opus. `ffmpeg` converts them to 16 kHz mono WAV. A file that is already `.wav` skips ffmpeg. |

Model tarball:

<https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2025-09-09.tar.bz2>

Unpack it so `model_dir` contains `model.int8.onnx` and `tokens.txt`. The default directory is `<home>/models/sensevoice` (`RUSTFOX_HOME`, then `[general].home`, then `~/.rustfox`).

Binaries (linux x64, build **with** TTS — not a `*-no-tts*` asset):

<https://github.com/k2-fsa/sherpa-onnx/releases/download/v2.0.1/sherpa-onnx-v2.0.1-linux-x64-shared.tar.bz2>

Extract that archive and put `sherpa-onnx-offline` and `sherpa-onnx-offline-tts` on `PATH`. The shared build also needs its `lib/` directory on `LD_LIBRARY_PATH` (`libonnxruntime.so`, `libsherpa-onnx-c-api.so`). `ffmpeg` is still required: voice notes become 16 kHz WAV, and TTS WAV becomes OGG/Opus.

```bash
curl -fsSL -o /tmp/sherpa-onnx-v2.0.1-linux-x64-shared.tar.bz2 \
  https://github.com/k2-fsa/sherpa-onnx/releases/download/v2.0.1/sherpa-onnx-v2.0.1-linux-x64-shared.tar.bz2 \
&& mkdir -p "$HOME/.local" \
&& tar -xjf /tmp/sherpa-onnx-v2.0.1-linux-x64-shared.tar.bz2 -C "$HOME/.local" \
&& export PATH="$HOME/.local/sherpa-onnx-v2.0.1-linux-x64-shared/bin:$PATH" \
&& export LD_LIBRARY_PATH="$HOME/.local/sherpa-onnx-v2.0.1-linux-x64-shared/lib:${LD_LIBRARY_PATH:-}"
```

Sherpa is invoked as a process. RustFox does not link onnxruntime, so the model and these binaries stay outside CI. Tests substitute a script for `stt_bin`.

CLI shape (the wav path is last):

```
sherpa-onnx-offline \
  --tokens=<model_dir>/tokens.txt \
  --sense-voice-model=<model_dir>/model.int8.onnx \
  --sense-voice-language=yue \
  --num-threads=2 \
  --debug=0 \
  <wav>
```

## Opt-in OpenRouter STT

```toml
[voice]
openrouter_stt = true
# openrouter_stt_model = "openai/whisper-large-v3"
```

The request is `POST {openrouter.base_url}/audio/transcriptions` with the existing OpenRouter API key. The body is JSON: `model` and `input_audio` (`data` base64, `format`). For `language = "yue"` the language field is omitted. There is no ISO-639-1 code `yue`, and forcing `zh` would bias the model toward Mandarin.

The flag defaults to **false**. An unknown `engine` value fails before any cloud call, even if the flag is true.

## Opt-in TTS

```toml
[voice]
tts = true
```

Default synthesis is `sherpa-onnx-offline-tts`, then `ffmpeg` with `libopus` to OGG/Opus. RustFox requires `model.onnx` and `tokens.txt` in `tts_model_dir`. `lexicon.txt` is optional (`--vits-lexicon` only when that file exists). `espeak-ng-data/` is optional. When `rule.fst` exists, the command adds `--tts-rule-fsts=<tts_model_dir>/rule.fst`. The default directory is `<home>/models/tts-cantonese`.

Canonical Cantonese pack: <https://huggingface.co/csukuangfj/vits-cantonese-hf-xiaomaiiwn>

Download these files into `<home>/models/tts-cantonese/` (`RUSTFOX_HOME`, then `[general].home`, then `~/.rustfox`):

| File | Role |
|------|------|
| `vits-cantonese-hf-xiaomaiiwn.onnx` | Weights, about 114MB. Symlink this file to `model.onnx`. |
| `tokens.txt` | Required. |
| `lexicon.txt` | Optional. Passed as `--vits-lexicon` when present. |
| `rule.fst` | Required by this pack. Passed as `--tts-rule-fsts` when present. |

Then symlink the weights to the name the loader checks:

```bash
cd <home>/models/tts-cantonese
ln -sf vits-cantonese-hf-xiaomaiiwn.onnx model.onnx
```

Spoken text is capped at 3500 characters. The chat text is not truncated.

`tts_bin` replaces the sherpa + ffmpeg pair. The contract is:

```
tts_bin --text-file <txt> --output <file.ogg>
```

OpenRouter `/audio/speech` is not wired.

## Config

See the commented `[voice]` block in [`config.example.toml`](../config.example.toml). An absent `[voice]` section is the same as the defaults: SenseVoice on, languages `yue` and `en` (CLI `auto`), `openrouter_stt = false`, `tts = false`.

| Key | Default | Meaning |
|-----|---------|---------|
| `engine` | `sensevoice` | Primary local STT. Only this id is implemented. |
| `model_dir` | `<home>/models/sensevoice` | SenseVoice `model.int8.onnx` or `model.onnx`, plus `tokens.txt`. |
| `stt_bin` | `sherpa-onnx-offline` | Local STT binary. |
| `ffmpeg_bin` | `ffmpeg` | OGG/Opus ↔ WAV. |
| `enabled` | `true` | Master gate. Absent means on. `false` skips STT and TTS. A caption-less note says voice is off. |
| `languages` | `yue`, `en` | Selection from `yue`, `zh`, `en`, `ja`, `ko`. One id is that CLI flag. Two or more is `auto`. Empty or unknown is rejected. |
| `language` | — | Legacy single id. Used only when `languages` is absent. An install pinned to `yue` stays on `yue` until Settings saves a selection. |
| `openrouter_stt` | `false` | Cloud transcription. Off means no network STT. |
| `openrouter_stt_model` | `openai/whisper-large-v3` | Used only when `openrouter_stt` is true. |
| `stt_timeout_secs` | `120` | Wall clock for one transcription. `0` becomes 120. |
| `tts` | `false` | Spoken copy of a voice-origin reply, in addition to text. Typed chat is not spoken. |
| `tts_bin` | empty | When set, this binary writes the OGG instead of sherpa + ffmpeg. |
| `tts_model_dir` | `<home>/models/tts-cantonese` | Cantonese VITS `model.onnx` and `tokens.txt`. Optional `lexicon.txt`. `rule.fst` is passed as `--tts-rule-fsts` when the file exists. |
| `tts_en_model_dir` | `<home>/models/tts-en` | English Piper pack `vits-piper-en_US-lessac-medium` (`model.onnx`, `tokens.txt`, optional `espeak-ng-data/`). Local sherpa TTS, not a cloud voice. Spoken only when the reply language is `en`. |
| `tts_timeout_secs` | `120` | Wall clock for one synthesis. `0` becomes 120. |

Missing model files do not stop startup. The next voice note gets the clarify (or the caption, if you typed one).
