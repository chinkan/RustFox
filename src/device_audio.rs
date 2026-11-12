//! ESP32 HTTP audio (ADR-0066).
//!
//! `POST /api/device/v1/audio` takes one 16 kHz mono WAV on the same bind as
//! the portal and Device WebSocket. Auth is the per-device Bearer token.
//! HTTP chunked transfer encoding is a normal request body. There is no
//! chunk-index protocol.
//!
//! SenseVoice (`transcribe_file`, language `auto`) produces the transcript.
//! That text is `user_text` on the owner's primary interactive TurnGate. It
//! is not a Telegram voice note and it is not voice-origin. An empty or
//! failed transcript does not start a turn.
//!
//! The text reply is still `assistant_text`. Spoken `yue` or `en` also sends
//! a JSON text `assistant_audio` frame. `GET /api/device/v1/audio/{id}`
//! returns 16 kHz mono PCM s16le for that device token only. PCM is the WAV
//! stage of the shared local TTS library, not OGG/Opus. Other languages stay
//! text-only. Reply length is not capped. If the uploader's socket is gone
//! before `assistant_audio`, the turn keeps the text and the PCM is not stored.
//!
//! Network assumptions stay ADR-0056 Decision 7: loopback, Tailscale, or a
//! trusted LAN. TLS stays on an external reverse proxy.

use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path as AxumPath, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get as route_get, post as route_post};
use axum::Json;
use axum::Router;
use serde_json::{json, Value};

use crate::asr::{
    self, spoken_reply_language, synthesize_pcm_s16le, transcribe_file, AsrError, AsrErrorKind,
    Transcript, TtsError, VoiceConfig, VOICE_LANG_ORDER,
};
use crate::device_ws::{
    self, ack_body, assistant_text_frame, error_body, header_auth, DeviceInbound, DeviceRecord,
    DeviceRegistry, DeviceTurnOutcome, DeviceUserText, HeaderAuth, LiveSockets,
};

pub const MAX_WAV_BYTES: usize = 8 * 1024 * 1024;

/// In-memory PCM for one notified `assistant_audio`. Not a replay queue.
#[derive(Clone, Default)]
pub struct PcmStore {
    inner: Arc<Mutex<std::collections::HashMap<String, Clip>>>,
}

struct Clip {
    device_id: String,
    pcm: Vec<u8>,
}

enum ClipLookup {
    Missing,
    Forbidden,
    Ready(Vec<u8>),
}

impl PcmStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, id: impl Into<String>, device_id: impl Into<String>, pcm: Vec<u8>) {
        self.lock().insert(
            id.into(),
            Clip {
                device_id: device_id.into(),
                pcm,
            },
        );
    }

    pub fn remove(&self, id: &str) {
        self.lock().remove(id);
    }

    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    fn lookup(&self, id: &str, device_id: &str) -> ClipLookup {
        match self.lock().get(id) {
            None => ClipLookup::Missing,
            Some(clip) if clip.device_id != device_id => ClipLookup::Forbidden,
            Some(clip) => ClipLookup::Ready(clip.pcm.clone()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, std::collections::HashMap<String, Clip>> {
        self.inner.lock().unwrap_or_else(|err| err.into_inner())
    }
}

/// STT and PCM for one upload. Production uses [`LibraryDeviceAudio`].
#[async_trait]
pub trait DeviceAudioEngine: Send + Sync {
    async fn transcribe(&self, wav: &Path) -> Result<Transcript, AsrError>;
    async fn synthesize_pcm(&self, text: &str, spoken_lang: &str) -> Result<Vec<u8>, TtsError>;
}

/// SenseVoice language `auto`, then the WAV/PCM stage of the local TTS library.
pub struct LibraryDeviceAudio {
    voice: VoiceConfig,
    openrouter_base: String,
    openrouter_key: String,
}

impl LibraryDeviceAudio {
    pub fn new(
        voice: VoiceConfig,
        openrouter_base: impl Into<String>,
        openrouter_key: impl Into<String>,
    ) -> Self {
        Self {
            voice,
            openrouter_base: openrouter_base.into(),
            openrouter_key: openrouter_key.into(),
        }
    }
}

#[async_trait]
impl DeviceAudioEngine for LibraryDeviceAudio {
    async fn transcribe(&self, wav: &Path) -> Result<Transcript, AsrError> {
        let voice = voice_for_device_upload(&self.voice);
        transcribe_file(&voice, &self.openrouter_base, &self.openrouter_key, wav).await
    }

    async fn synthesize_pcm(&self, text: &str, spoken_lang: &str) -> Result<Vec<u8>, TtsError> {
        synthesize_pcm_s16le(&self.voice, spoken_lang, text).await
    }
}

/// This upload always asks SenseVoice for `auto`. OpenRouter stays whatever
/// the operator already opted into. `normalize` is not allowed to put a
/// single language back.
pub fn voice_for_device_upload(voice: &VoiceConfig) -> VoiceConfig {
    let mut voice = voice.clone();
    voice.languages = VOICE_LANG_ORDER
        .iter()
        .map(|lang| (*lang).to_string())
        .collect();
    voice.normalize();
    voice
}

/// Shared by the standalone device router and the portal handlers.
#[derive(Clone)]
pub struct AudioHost {
    pub registry: DeviceRegistry,
    pub inbound: Arc<dyn DeviceInbound>,
    pub live: LiveSockets,
    pub pcm: PcmStore,
    pub engine: Arc<dyn DeviceAudioEngine>,
}

pub fn router(host: AudioHost) -> Router {
    Router::new()
        .route(
            device_ws::AUDIO_PATH,
            route_post(post_route).layer(DefaultBodyLimit::max(MAX_WAV_BYTES)),
        )
        .route(device_ws::AUDIO_ID_PATH, route_get(get_route))
        .with_state(host)
}

async fn post_route(State(host): State<AudioHost>, headers: HeaderMap, body: Bytes) -> Response {
    post(host, headers, body).await
}

async fn get_route(
    State(host): State<AudioHost>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
) -> Response {
    get(&host.registry, &host.pcm, &id, &headers).await
}

pub async fn post(host: AudioHost, headers: HeaderMap, body: Bytes) -> Response {
    let device = match header_auth(&host.registry, &headers).await {
        HeaderAuth::Device(device) => device,
        HeaderAuth::Absent => return unauthorized(),
        HeaderAuth::Reject(response) => return response,
    };
    if !is_wav_content_type(&headers) {
        return error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_media_type",
            "Content-Type must be audio/wav",
        );
    }
    if body.len() > MAX_WAV_BYTES {
        return error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_large",
            "audio upload is too large",
        );
    }
    if !is_upload_wav(&body) {
        return error(
            StatusCode::BAD_REQUEST,
            "bad_format",
            "expected 16 kHz mono PCM WAV",
        );
    }

    let dir = std::env::temp_dir().join(format!("rustfox-device-audio-{}", uuid::Uuid::new_v4()));
    if let Err(err) = tokio::fs::create_dir_all(&dir).await {
        tracing::warn!(error = %err, "device audio work dir");
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "could not store the upload",
        );
    }
    let path = dir.join("upload.wav");
    if let Err(err) = tokio::fs::write(&path, &body).await {
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tracing::warn!(error = %err, "device audio write");
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "could not store the upload",
        );
    }
    let transcribed = host.engine.transcribe(&path).await;
    let _ = tokio::fs::remove_dir_all(&dir).await;
    let transcript = match transcribed {
        Ok(transcript) if !transcript.text.trim().is_empty() => transcript,
        Ok(_) => {
            return error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "empty_transcript",
                "empty transcript",
            );
        }
        Err(err) if err.kind == AsrErrorKind::Empty => {
            tracing::info!(error = %err, "device audio transcript was empty");
            return error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "empty_transcript",
                "empty transcript",
            );
        }
        Err(err) => {
            tracing::warn!(error = %err, "device audio transcription failed");
            return error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "transcription_failed",
                "transcription failed",
            );
        }
    };

    let text = transcript.text.trim().to_string();
    let (device_id, room, fw) = turn_fields(&host.live, &device);
    let msg = DeviceUserText {
        text: text.clone(),
        device_id,
        room,
        fw_version: fw,
        token_device_id: device.id.clone(),
    };
    let outcome = match host.inbound.submit(msg).await {
        Ok(outcome) => outcome,
        Err(err) => {
            tracing::warn!(error = %err, device_id = %device.id, "device audio turn failed");
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "agent",
                "the assistant turn failed",
            );
        }
    };

    match outcome {
        DeviceTurnOutcome::Queued => {
            let _ = host.live.try_send(
                &device.id,
                &ack_body("queued", &device.id, device.room.as_deref()),
            );
            Json(json!({
                "transcript": text,
                "status": "queued",
                "replies": [],
                "audio": [],
            }))
            .into_response()
        }
        DeviceTurnOutcome::QueueFull => {
            let _ = host.live.try_send(
                &device.id,
                &error_body("queue_full", "too many messages are waiting"),
            );
            error(
                StatusCode::TOO_MANY_REQUESTS,
                "queue_full",
                "too many messages are waiting",
            )
        }
        DeviceTurnOutcome::Replies(replies) => {
            let audio = notify(&host, &device, &transcript, &replies).await;
            Json(json!({
                "transcript": text,
                "replies": replies,
                "audio": audio,
            }))
            .into_response()
        }
    }
}

pub async fn get(
    registry: &DeviceRegistry,
    pcm: &PcmStore,
    id: &str,
    headers: &HeaderMap,
) -> Response {
    let device = match header_auth(registry, headers).await {
        HeaderAuth::Device(device) => device,
        HeaderAuth::Absent => return unauthorized(),
        HeaderAuth::Reject(response) => return response,
    };
    match pcm.lookup(id, &device.id) {
        ClipLookup::Missing => error(StatusCode::NOT_FOUND, "not_found", "audio not found"),
        ClipLookup::Forbidden => unauthorized(),
        ClipLookup::Ready(bytes) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/octet-stream")],
            bytes,
        )
            .into_response(),
    }
}

async fn notify(
    host: &AudioHost,
    device: &DeviceRecord,
    transcript: &Transcript,
    replies: &[String],
) -> Vec<Value> {
    let spoken = spoken_reply_language("auto", transcript.detected_lang.as_deref());
    let mut audio = Vec::new();
    for reply in replies {
        if !host.live.try_send(&device.id, &assistant_text_frame(reply)) {
            break;
        }
        let Some(lang) = spoken.as_deref() else {
            continue;
        };
        if reply.trim().is_empty() {
            continue;
        }
        let pcm = match host.engine.synthesize_pcm(reply, lang).await {
            Ok(pcm) if !pcm.is_empty() => pcm,
            Ok(_) => {
                tracing::warn!("device TTS returned empty audio; text reply is kept");
                continue;
            }
            Err(err) => {
                tracing::warn!(error = %err, "device TTS failed; text reply is kept");
                continue;
            }
        };
        let id = uuid::Uuid::new_v4().to_string();
        let url = format!("/api/device/v1/audio/{id}");
        let frame = json!({
            "type": "assistant_audio",
            "id": id,
            "url": url,
            "sample_rate": 16000,
            "channels": 1,
            "encoding": "s16le",
        });
        host.pcm.insert(id.clone(), device.id.clone(), pcm);
        if host.live.try_send(&device.id, &frame) {
            audio.push(json!({
                "id": id,
                "url": url,
                "sample_rate": 16000,
                "channels": 1,
                "encoding": "s16le",
            }));
        } else {
            host.pcm.remove(&id);
        }
    }
    audio
}

fn turn_fields(
    live: &LiveSockets,
    device: &DeviceRecord,
) -> (String, Option<String>, Option<String>) {
    if let Some(meta) = live.meta(&device.id) {
        let device_id = if meta.device_id.trim().is_empty() {
            device.id.clone()
        } else {
            meta.device_id
        };
        let room = meta
            .room
            .filter(|room| !room.trim().is_empty())
            .or_else(|| device.room.clone());
        let fw = meta.fw.filter(|fw| !fw.trim().is_empty());
        (device_id, room, fw)
    } else {
        (device.id.clone(), device.room.clone(), None)
    }
}

fn is_wav_content_type(headers: &HeaderMap) -> bool {
    let Some(value) = headers.get(header::CONTENT_TYPE) else {
        return false;
    };
    let Ok(value) = value.to_str() else {
        return false;
    };
    let mime = value.split(';').next().unwrap_or("").trim();
    mime.eq_ignore_ascii_case("audio/wav")
}

fn is_upload_wav(bytes: &[u8]) -> bool {
    match asr::parse_wav_pcm(bytes) {
        Ok(wav) => wav.sample_rate == 16_000 && wav.channels == 1 && !wav.pcm.is_empty(),
        Err(_) => false,
    }
}

fn unauthorized() -> Response {
    error(
        StatusCode::UNAUTHORIZED,
        "unauthorized",
        "invalid device token",
    )
}

fn error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({
            "error": { "code": code, "message": message }
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_audio_does_not_use_the_telegram_voice_pipe() {
        let src = include_str!("device_audio.rs");
        let prod = src.split("mod tests").next().unwrap();
        for needle in [
            "plan_reply_delivery",
            "tts_spoken_text",
            "sendVoice",
            "getFile",
            "shape_voice_note",
            "telegram_voice",
            "voice_origin: true",
            "ffmpeg_ogg_opus_args",
        ] {
            assert!(!prod.contains(needle), "{needle} must stay off this path");
        }
    }

    #[test]
    fn upload_voice_forces_sensevoice_auto_without_enabling_cloud() {
        let mut voice = VoiceConfig {
            languages: vec!["ja".into()],
            openrouter_stt: false,
            ..VoiceConfig::default()
        };
        voice.normalize();
        assert_eq!(voice.stt_language_flag(), "ja");
        let forced = voice_for_device_upload(&voice);
        assert_eq!(forced.stt_language_flag(), "auto");
        assert!(!forced.openrouter_stt);
        assert!(!voice.openrouter_stt);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn library_transcribe_passes_sensevoice_language_auto() {
        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join("model");
        std::fs::create_dir_all(&model).unwrap();
        std::fs::write(model.join("model.int8.onnx"), b"x").unwrap();
        std::fs::write(model.join("tokens.txt"), b"a\n").unwrap();
        let args_path = dir.path().join("args.txt");
        let script = dir.path().join("stt.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nprintf '%s\\n' '{{\"lang\":\"en\",\"text\":\"lights\"}}'\n",
                args_path.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
        let wav = dir.path().join("upload.wav");
        std::fs::write(&wav, b"RIFFfake").unwrap();
        let voice = VoiceConfig {
            languages: vec!["ja".into()],
            model_dir: model,
            stt_bin: script.display().to_string(),
            openrouter_stt: false,
            ..VoiceConfig::default()
        };
        let engine = LibraryDeviceAudio::new(voice, "http://127.0.0.1:9", "");
        let transcript = engine.transcribe(&wav).await.unwrap();
        assert_eq!(transcript.text, "lights");
        assert_eq!(transcript.source, crate::asr::SttSource::SenseVoice);
        let args = std::fs::read_to_string(&args_path).unwrap();
        assert!(args.contains("--sense-voice-language=auto"), "{args}");
        assert!(!args.contains("--sense-voice-language=ja"), "{args}");
    }
}
