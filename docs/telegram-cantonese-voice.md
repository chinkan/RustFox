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

A typed caption is kept after a blank line. If the note has no caption and download or transcription fails, the bot sends a short clarify and asks you to type the message. It does not return without saying anything.

OpenRouter speech-to-text does not run unless you set `openrouter_stt = true`. A missing local model with the flag left at the default never posts audio to the network.

TTS does not run unless you set `tts = true`. When it is on, the reply is the usual text message **and** a `sendVoice` OGG/Opus note. Synthesis or `sendVoice` failure leaves the text in the chat.

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

Sherpa is invoked as a process. RustFox does not link onnxruntime, so CI does not download the model. Tests substitute a script for `stt_bin`.

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

Default synthesis is `sherpa-onnx-offline-tts` (VITS: `model.onnx` and `tokens.txt` in `tts_model_dir`, optional `lexicon.txt` or `espeak-ng-data`) and then `ffmpeg` with `libopus` to OGG/Opus. The default directory is `<home>/models/tts-cantonese`.

Spoken text is capped at 3500 characters. The chat text is not truncated.

`tts_bin` replaces the sherpa + ffmpeg pair. The contract is:

```
tts_bin --text-file <txt> --output <file.ogg>
```

OpenRouter `/audio/speech` is not wired.

## Config

See the commented `[voice]` block in [`config.example.toml`](../config.example.toml). An absent `[voice]` section is the same as the defaults: SenseVoice, `yue`, `openrouter_stt = false`, `tts = false`.

| Key | Default | Meaning |
|-----|---------|---------|
| `engine` | `sensevoice` | Primary local STT. Only this id is implemented. |
| `model_dir` | `<home>/models/sensevoice` | SenseVoice `model.int8.onnx` or `model.onnx`, plus `tokens.txt`. |
| `stt_bin` | `sherpa-onnx-offline` | Local STT binary. |
| `ffmpeg_bin` | `ffmpeg` | OGG/Opus ↔ WAV. |
| `language` | `yue` | SenseVoice language. Unknown values become `yue`. |
| `openrouter_stt` | `false` | Cloud transcription. Off means no network STT. |
| `openrouter_stt_model` | `openai/whisper-large-v3` | Used only when `openrouter_stt` is true. |
| `stt_timeout_secs` | `120` | Wall clock for one transcription. `0` becomes 120. |
| `tts` | `false` | Spoken reply in addition to text. |
| `tts_bin` | empty | When set, this binary writes the OGG instead of sherpa + ffmpeg. |
| `tts_model_dir` | `<home>/models/tts-cantonese` | VITS `model.onnx` and `tokens.txt`. |
| `tts_timeout_secs` | `120` | Wall clock for one synthesis. `0` becomes 120. |

Missing model files do not stop startup. The next voice note gets the clarify (or the caption, if you typed one).
