# ADR-0066: ESP32 off-device STT and HTTP audio (path B)

## Status
Accepted (Product tip Final ACCEPT 2026-11-12 at `a0f3b9b`, squash #408; Should / Soft parallel). Does not block the v1.0.9 tag. No Must claim. No runtime in the accepted tip. No tag.

## Date
2026-11-12

## Context
Kan (2026-11-12) asked to add touch and talk, and to use a same-screen-size image of the official RustFox logo on the board. He did not write Must and did not name a milestone.

[#353](https://github.com/chinkan/RustFox/issues/353) (closed; do not reopen or close it from this work) already locked **path B** for the Waveshare ESP32-C6: the board captures audio, ASR is off-device, and the C6 does not run open-vocab STT (no PSRAM). That lock also dropped Xiaozhi cloud ASR and Opus. Firmware stays in [`chinkan/rustfox-esp32-c6`](https://github.com/chinkan/rustfox-esp32-c6). Logo, touch, I2S, and press-and-hold UX are [rustfox-esp32-c6#2](https://github.com/chinkan/rustfox-esp32-c6/issues/2). This monorepo slice is [#407](https://github.com/chinkan/RustFox/issues/407): this ADR first, then a later server implementation after the ADR is accepted. The docs PR does not close #407.

Before this amend, [ADR-0056](./0056-device-websocket-channel.md) Decision 6 said the ESP32 does on-device/edge STT and sends text, and that RustFox may later return TTS/PCM for the board. That wording contradicted path B. Device WS on main is JSON text only; binary frames are rejected. [ADR-0058](./0058-telegram-cantonese-stt-tts.md) still lists binary Device WS audio as out of scope and keeps the board mic off the Telegram voice download / `sendVoice` path. Those Telegram rules stay. This ADR adds the HTTP side channel they left unspecified.

ADR-0056 Decisions 1–5 and 7 stay in force: opt-in `[device_ws]`, shared portal bind, `GET /api/device/v1/ws`, the device as an adapter into the owner primary interactive TurnGate, the per-device token, JSON text framing for the v1.0.9 MVP, and loopback / Tailscale / trusted LAN.

## Decision
**Path B.** Should / Soft parallel. The ESP32-C6 board captures and plays TTS. RustFox does STT. This does not block the v1.0.9 tag.

1. **Who hears and who speaks.** The board captures the mic and plays TTS on the ES8311. RustFox runs speech-to-text. There is no on-device open-vocab STT on the C6.
2. **Device WS stays a JSON text control plane.** Frames remain `auth`, `user_text`, `assistant_text`, `device_ack`, `ping` / `pong`, and `error`, plus the optional `device_hello` already in ADR-0056 Decision 5. Binary audio frames stay rejected. Partial token streaming stays out of scope. This decision adds one JSON **text** frame, `assistant_audio` (Decision 5 below). It is not a binary frame and does not replace Decision 5's v1.0.9 MVP list.
3. **Upload after press-and-hold release.** The device uploads one 16 kHz mono WAV on the same Axum bind as the portal and Device WS (ADR-0056 Decisions 1–2). Auth is the same per-device token as Device WS, sent as `Authorization: Bearer <token>`. That token is not the portal login token.
   - `POST /api/device/v1/audio`
   - `Content-Type: audio/wav`
   - The body is the WAV (one request). HTTP chunked transfer encoding is fine. A separate chunk-index protocol is not part of this ADR.
   - The route exists when `[device_ws]` is on, including the headless API-only process. Portal UI may be off.
4. **STT, then the existing `user_text` path.** The server transcribes with `SenseVoiceCli` / `transcribe_file` and SenseVoice language `auto` (`--sense-voice-language=auto`) for this upload. The input is 16 kHz mono WAV, or a file the existing ffmpeg helper can convert to 16 kHz mono WAV. The transcript is then submitted as `user_text` on the owner's primary interactive TurnGate (ADR-0056 Decision 3). That is the same owner primary conversation. It is not a new Portal chat and it is not a Telegram turn. Do not wrap it as a Telegram `[Voice note]:` and do not mark it voice-origin for `sendVoice`. A call to `transcribe_file` does not add a device-only cloud default; OpenRouter STT stays the existing opt-in inside that function. An empty or failed transcript does not start a turn.
5. **TTS downlink is 16 kHz mono PCM (s16le) for the ES8311**, over authenticated HTTP. Do not send OGG/Opus. Do not call Telegram `getFile`, `sendVoice`, or `plan_reply_delivery`.
   - The text reply is still the existing `assistant_text` frame.
   - When PCM is ready, the server also sends a JSON text frame on that device's socket:

     ```json
     {
       "type": "assistant_audio",
       "id": "<id>",
       "url": "/api/device/v1/audio/<id>",
       "sample_rate": 16000,
       "channels": 1,
       "encoding": "s16le"
     }
     ```

   - `GET /api/device/v1/audio/{id}` with the same device Bearer token returns the raw PCM bytes (`application/octet-stream`, 16 kHz, mono, s16le). Chunked transfer encoding is fine. The `url` is a path on the same bind the device already uses.
   - The notifying socket is the uploader's Device WS. GET accepts that device's token and rejects any other token.
   - PCM comes from the shared local TTS library (the WAV / PCM stage). This path does not run the OGG/Opus encode used for Telegram `sendVoice`.
6. **Shared library, separate pipes.** The device path may share the SenseVoice / local TTS library ([ADR-0058](./0058-telegram-cantonese-stt-tts.md) Decision 5). It must not share Telegram voice ingress or `sendVoice` egress.
7. **Firmware stays out of this monorepo.** Logo (official asset at 480×480), touch, I2S, and press-and-hold UX are specified in [rustfox-esp32-c6#2](https://github.com/chinkan/rustfox-esp32-c6/issues/2). This ADR does not choose a touch controller, GPIO map, or blit strategy.
8. **Priority.** Should / Soft parallel. No Must claim. No tag. Server routes are a later change on #407, after this ADR is accepted. This document does not implement them.

Network assumptions for the new routes are ADR-0056 Decision 7: loopback, Tailscale, or a trusted LAN. TLS stays on an external reverse proxy. Bare public HTTP for device audio is not a supported deployment.

## Rejected
- On-device open-vocab STT on the ESP32-C6.
- Binary audio frames on Device WS.
- Routing the board mic or speaker through the Telegram voice pipeline (`getFile`, voice-note ingress, `sendVoice`, `plan_reply_delivery`, OGG/Opus).
- Putting firmware in this monorepo.
- A third Channel, or a new Portal conversation, for device speech.
- Xiaozhi cloud ASR / Opus as the board protocol.
- Treating this work as a v1.0.9 Must or blocking the v1.0.9 tag.

## Consequences
- Amends [ADR-0056](./0056-device-websocket-channel.md) Decision 6 only. Decisions 1–5 and 7 stay, including "Framing (v1.0.9 MVP): JSON text frames only" and "Out of scope: binary audio frames, partial token streaming." `assistant_audio` is an additional JSON text frame from this ADR.
- [ADR-0058](./0058-telegram-cantonese-stt-tts.md) keeps binary Device WS audio out of scope. Telegram STT/TTS behaviour is unchanged. A one-line cross-reference points ESP32 audio here.
- Later server work adds `POST /api/device/v1/audio` and `GET /api/device/v1/audio/{id}` on the existing device bind and reuses the ADR-0056 per-device token check. This docs change ships neither route.
- Spoken device replies are limited to what the shared local TTS library already renders (today, spoken `yue` or `en` on that library). Other languages still return `assistant_text` and omit `assistant_audio`. Reply length is not capped here.
- If the uploader's socket is gone before `assistant_audio`, the owner turn still keeps the text. This ADR does not add a PCM replay queue.
- Firmware implementation waits on acceptance of this ADR and lands in rustfox-esp32-c6, not here.
- #407 stays open for the server slice. #353 stays closed and is not reopened.
