//! Telegram voice-note ingress and spoken replies (ADR-0058).
//!
//! Download uses the Bot API (`getFile` + file fetch) and stays in this
//! platform adapter. The shareable ASR library does not import it. Device
//! WebSocket and ESP32 capture must not call [`decide_voice_turn`] or
//! [`maybe_send_tts_voice`].
//!
//! A transcribed note is voice-origin ([`voice_origin_from_turn`]). The agent
//! then injects the voice-mode prompt. [`maybe_send_tts_voice`] speaks only
//! that reply. The model is not asked to run STT or TTS.

use std::path::PathBuf;

use anyhow::{Context, Result};
use teloxide::prelude::*;
use teloxide::types::ChatId;
use tracing::{info, warn};

use crate::asr::{
    plan_reply_delivery, shape_voice_note, spoken_reply_language, transcribe_file,
    voice_note_clarify, AsrError, LocalTts, VoiceConfig, VOICE_OFF_CLARIFY,
};

/// What to do with one Telegram voice note after download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum VoiceTurn {
    /// Shaped text for the interactive TurnGate lane.
    /// `spoken_lang` is `yue` or `en` when this reply may be spoken.
    Text {
        body: String,
        spoken_lang: Option<String>,
    },
    /// Tell the user and do not enter the turn (voice-only failure, or voice off).
    Clarify(String),
    /// Tell the user the audio failed, then continue with the typed caption.
    ClarifyThen { clarify: String, text: String },
    /// `enabled` is false and the note has a caption. Ordinary typed text.
    Plain(String),
}

/// Outcome of the Bot API download. The download itself stays in `telegram`.
#[derive(Debug)]
pub(crate) enum DownloadedVoice {
    Ready(PathBuf),
    Failed(String),
}

/// Successful transcription is the only voice-origin turn. A clarify, including
/// one that keeps a typed caption, is not.
pub(crate) fn voice_origin_from_turn(turn: &VoiceTurn) -> bool {
    matches!(turn, VoiceTurn::Text { .. })
}

pub(crate) fn spoken_lang_from_turn(turn: &VoiceTurn) -> Option<&str> {
    match turn {
        VoiceTurn::Text { spoken_lang, .. } => spoken_lang.as_deref(),
        _ => None,
    }
}

/// Spoken copy requires a voice-origin reply, the master gate, TTS, and a
/// spoken language of `yue` or `en`. Typed chat stays text-only.
pub(crate) fn tts_for_turn(
    voice_origin: bool,
    enabled: bool,
    tts_enabled: bool,
    spoken_lang: Option<&str>,
) -> bool {
    voice_origin && enabled && tts_enabled && matches!(spoken_lang, Some("yue" | "en"))
}

/// Master gate is off. No download and no STT. A caption stays typed text.
pub(crate) fn voice_disabled_turn(caption: &str) -> VoiceTurn {
    let caption = caption.trim();
    if caption.is_empty() {
        VoiceTurn::Clarify(VOICE_OFF_CLARIFY.to_string())
    } else {
        VoiceTurn::Plain(caption.to_string())
    }
}

/// `[Voice note]:` plus the transcript. A non-empty caption is kept after a
/// blank line so typed words are not dropped.
pub(crate) fn voice_turn_text(transcript: &str, caption: &str) -> String {
    let shaped = shape_voice_note(transcript);
    let caption = caption.trim();
    if caption.is_empty() {
        shaped
    } else {
        format!("{shaped}\n\n{caption}")
    }
}

fn failure_turn(err: AsrError, caption: &str) -> VoiceTurn {
    let clarify = voice_note_clarify(&err);
    let caption = caption.trim();
    if caption.is_empty() {
        VoiceTurn::Clarify(clarify)
    } else {
        VoiceTurn::ClarifyThen {
            clarify,
            text: caption.to_string(),
        }
    }
}

/// Map a downloaded voice file to TurnGate text or an honest clarify.
///
/// A failed download or transcription of a caption-less note is
/// [`VoiceTurn::Clarify`], never an empty success.
pub(crate) async fn decide_voice_turn(
    downloaded: DownloadedVoice,
    caption: &str,
    voice_cfg: &VoiceConfig,
    openrouter_base: &str,
    openrouter_key: &str,
) -> VoiceTurn {
    if !voice_cfg.enabled {
        return voice_disabled_turn(caption);
    }
    let path = match downloaded {
        DownloadedVoice::Failed(reason) => {
            let err = AsrError::download(reason);
            warn!(error = %err, "telegram voice download failed");
            return failure_turn(err, caption);
        }
        DownloadedVoice::Ready(path) => path,
    };
    match transcribe_file(voice_cfg, openrouter_base, openrouter_key, &path).await {
        Ok(transcript) => {
            info!(
                chars = transcript.text.chars().count(),
                source = ?transcript.source,
                "voice note transcribed"
            );
            let spoken_lang = spoken_reply_language(
                &voice_cfg.stt_language_flag(),
                transcript.detected_lang.as_deref(),
            );
            VoiceTurn::Text {
                body: voice_turn_text(&transcript.text, caption),
                spoken_lang,
            }
        }
        Err(err) => {
            warn!(
                error = %err,
                cloud = err.cloud_attempted,
                "voice note transcription failed"
            );
            failure_turn(err, caption)
        }
    }
}

/// Send an OGG/Opus voice note. The caller has already sent `text`.
/// This function never replaces that text copy.
pub(crate) async fn maybe_send_tts_voice(
    bot: &Bot,
    chat_id: ChatId,
    voice: &VoiceConfig,
    text: &str,
    voice_origin: bool,
    spoken_lang: Option<&str>,
) {
    if !tts_for_turn(voice_origin, voice.enabled, voice.tts, spoken_lang) || text.trim().is_empty()
    {
        return;
    }
    let synth = LocalTts {
        cfg: voice.clone(),
        spoken_lang: spoken_lang.map(str::to_string),
    };
    let delivery = plan_reply_delivery(true, text, Some(&synth)).await;
    debug_assert_eq!(delivery.text, text);
    let Some(ogg) = delivery.voice_ogg else {
        return;
    };
    if let Err(err) = send_ogg_voice(bot, chat_id, &ogg).await {
        warn!(error = %err, "sendVoice failed; text reply was already sent");
    }
}

async fn send_ogg_voice(bot: &Bot, chat_id: ChatId, ogg: &[u8]) -> Result<()> {
    let dir = std::env::temp_dir().join(format!("rustfox-voice-out-{}", uuid::Uuid::new_v4()));
    tokio::fs::create_dir_all(&dir)
        .await
        .context("create voice out dir")?;
    let path = dir.join("reply.ogg");
    tokio::fs::write(&path, ogg)
        .await
        .context("write ogg reply")?;
    let file = teloxide::types::InputFile::file(&path);
    let sent = bot.send_voice(chat_id, file).await;
    let _ = tokio::fs::remove_dir_all(&dir).await;
    sent.context("sendVoice")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asr::VoiceConfig;

    #[test]
    fn only_a_transcribed_voice_turn_is_voice_origin() {
        assert!(voice_origin_from_turn(&VoiceTurn::Text {
            body: "[Voice note]:\n早晨".into(),
            spoken_lang: Some("yue".into()),
        }));
        assert!(!voice_origin_from_turn(&VoiceTurn::Clarify(
            "couldn't download".into()
        )));
        assert!(!voice_origin_from_turn(&VoiceTurn::ClarifyThen {
            clarify: "couldn't download".into(),
            text: "remember this".into(),
        }));
        assert!(!voice_origin_from_turn(&VoiceTurn::Plain(
            "remember this".into()
        )));
    }

    #[test]
    fn tts_applies_only_to_yue_or_en_voice_origin_replies() {
        assert!(tts_for_turn(true, true, true, Some("yue")));
        assert!(tts_for_turn(true, true, true, Some("en")));
        assert!(
            !tts_for_turn(false, true, true, Some("en")),
            "typed chat must not sendVoice"
        );
        assert!(
            !tts_for_turn(true, false, true, Some("yue")),
            "master gate off"
        );
        assert!(!tts_for_turn(true, true, false, Some("en")));
        assert!(!tts_for_turn(true, true, true, Some("zh")));
        assert!(!tts_for_turn(true, true, true, Some("ja")));
        assert!(!tts_for_turn(true, true, true, Some("ko")));
        assert!(
            !tts_for_turn(true, true, true, None),
            "unresolved auto stays text"
        );
    }

    #[test]
    fn voice_turn_text_shapes_the_transcript_and_keeps_a_caption() {
        assert_eq!(voice_turn_text("早晨", ""), "[Voice note]:\n早晨");
        assert_eq!(
            voice_turn_text("  早晨  ", "  remember this  "),
            "[Voice note]:\n早晨\n\nremember this"
        );
    }

    #[tokio::test]
    async fn voice_only_download_failure_is_a_clarify_not_silence() {
        let turn = decide_voice_turn(
            DownloadedVoice::Failed("status 404".into()),
            "",
            &VoiceConfig::default(),
            "http://127.0.0.1:9",
            "",
        )
        .await;
        match turn {
            VoiceTurn::Clarify(message) => {
                assert!(message.contains("voice note"), "{message}");
                assert!(message.contains("couldn't download"), "{message}");
                assert!(message.contains("type your message"), "{message}");
                assert!(!message.contains("404"), "{message}");
            }
            other => panic!("expected clarify, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn voice_download_failure_keeps_a_typed_caption() {
        let turn = decide_voice_turn(
            DownloadedVoice::Failed("status 404".into()),
            " remember this ",
            &VoiceConfig::default(),
            "http://127.0.0.1:9",
            "",
        )
        .await;
        match turn {
            VoiceTurn::ClarifyThen { clarify, text } => {
                assert!(clarify.contains("couldn't download"));
                assert_eq!(text, "remember this");
            }
            other => panic!("expected clarify-then-caption, got {other:?}"),
        }
    }

    #[test]
    fn device_socket_does_not_call_telegram_voice_ingress() {
        let src = include_str!("../device_ws.rs");
        assert!(
            !src.contains("telegram_voice"),
            "device websocket must not share Telegram voice ingress"
        );
        assert!(!src.contains("decide_voice_turn"));
        assert!(!src.contains("download_telegram_file"));
        assert!(!src.contains("maybe_send_tts_voice"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ready_wav_is_shaped_for_the_turn() {
        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join("model");
        std::fs::create_dir_all(&model).unwrap();
        std::fs::write(model.join("model.int8.onnx"), b"x").unwrap();
        std::fs::write(model.join("tokens.txt"), b"a\n").unwrap();
        let script = dir.path().join("stt.sh");
        std::fs::write(&script, "#!/bin/sh\nprintf '%s\\n' '早晨'\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
        let audio = dir.path().join("note.wav");
        std::fs::write(&audio, b"RIFFfake").unwrap();

        let cfg = VoiceConfig {
            model_dir: model,
            stt_bin: script.display().to_string(),
            ..VoiceConfig::default()
        };
        assert!(!cfg.openrouter_stt);

        let turn = decide_voice_turn(
            DownloadedVoice::Ready(audio),
            "",
            &cfg,
            "http://127.0.0.1:9",
            "",
        )
        .await;
        match turn {
            VoiceTurn::Text { body, spoken_lang } => {
                assert_eq!(body, "[Voice note]:\n早晨");
                // Default selection is yue+en, so the CLI flag is auto.
                // The script does not report a language, so the reply stays text.
                assert_eq!(spoken_lang, None);
            }
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn disabled_voice_skips_stt_and_clarifies_a_captionless_note() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("stt.sh");
        std::fs::write(&script, "#!/bin/sh\nexit 1\n").unwrap();
        let cfg = VoiceConfig {
            enabled: false,
            openrouter_stt: true,
            stt_bin: script.display().to_string(),
            tts: true,
            ..VoiceConfig::default()
        };
        let turn = decide_voice_turn(
            DownloadedVoice::Ready(dir.path().join("missing.wav")),
            "",
            &cfg,
            "http://127.0.0.1:9",
            "sk-test",
        )
        .await;
        match turn {
            VoiceTurn::Clarify(message) => {
                assert!(message.to_ascii_lowercase().contains("voice"));
                assert!(
                    message.to_ascii_lowercase().contains("off") || message.contains("turned off"),
                    "{message}"
                );
                assert!(message.contains("type your message"), "{message}");
                assert!(!message.contains("transcribe"), "{message}");
                assert!(!message.contains("OpenRouter"), "{message}");
            }
            other => panic!("expected voice-off clarify, got {other:?}"),
        }
        assert!(!voice_origin_from_turn(&VoiceTurn::Clarify("x".into())));
    }

    #[tokio::test]
    async fn disabled_voice_keeps_a_caption_as_typed_text() {
        let cfg = VoiceConfig {
            enabled: false,
            tts: true,
            ..VoiceConfig::default()
        };
        let turn = decide_voice_turn(
            DownloadedVoice::Failed("should not matter".into()),
            " remember this ",
            &cfg,
            "http://127.0.0.1:9",
            "",
        )
        .await;
        assert_eq!(turn, VoiceTurn::Plain("remember this".into()));
        assert!(!voice_origin_from_turn(&turn));
        assert!(!tts_for_turn(false, cfg.enabled, cfg.tts, None));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn single_english_selection_marks_the_reply_spoken() {
        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join("model");
        std::fs::create_dir_all(&model).unwrap();
        std::fs::write(model.join("model.int8.onnx"), b"x").unwrap();
        std::fs::write(model.join("tokens.txt"), b"a\n").unwrap();
        let script = dir.path().join("stt.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\nprintf '%s\\n' '{\"lang\":\"zh\",\"text\":\"hello\"}'\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
        let audio = dir.path().join("note.wav");
        std::fs::write(&audio, b"RIFFfake").unwrap();
        let cfg = VoiceConfig {
            languages: vec!["en".into()],
            model_dir: model,
            stt_bin: script.display().to_string(),
            tts: true,
            ..VoiceConfig::default()
        };
        let turn = decide_voice_turn(
            DownloadedVoice::Ready(audio),
            "",
            &cfg,
            "http://127.0.0.1:9",
            "",
        )
        .await;
        match turn {
            VoiceTurn::Text { body, spoken_lang } => {
                assert_eq!(body, "[Voice note]:\nhello");
                assert_eq!(spoken_lang.as_deref(), Some("en"));
                assert!(tts_for_turn(
                    true,
                    cfg.enabled,
                    cfg.tts,
                    spoken_lang.as_deref()
                ));
            }
            other => panic!("expected spoken english turn, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn auto_zh_ja_ko_stay_text_only() {
        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join("model");
        std::fs::create_dir_all(&model).unwrap();
        std::fs::write(model.join("model.int8.onnx"), b"x").unwrap();
        std::fs::write(model.join("tokens.txt"), b"a\n").unwrap();
        let script = dir.path().join("stt.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\nprintf '%s\\n' '{\"lang\":\"zh\",\"text\":\"你好\"}'\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
        let audio = dir.path().join("note.wav");
        std::fs::write(&audio, b"RIFFfake").unwrap();
        let cfg = VoiceConfig {
            model_dir: model,
            stt_bin: script.display().to_string(),
            tts: true,
            ..VoiceConfig::default()
        };
        assert_eq!(cfg.stt_language_flag(), "auto");
        let turn = decide_voice_turn(
            DownloadedVoice::Ready(audio),
            "",
            &cfg,
            "http://127.0.0.1:9",
            "",
        )
        .await;
        match turn {
            VoiceTurn::Text { spoken_lang, .. } => {
                assert_eq!(spoken_lang, None);
                assert!(!tts_for_turn(true, true, true, spoken_lang.as_deref()));
            }
            other => panic!("expected text-only turn, got {other:?}"),
        }
    }
}
