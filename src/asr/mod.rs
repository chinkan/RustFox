//! Local speech library (ADR-0058).
//!
//! Primary engine: **SenseVoice** (`sherpa-onnx-offline`) with a Cantonese
//! (`yue`) model. OpenRouter `POST /audio/transcriptions` runs only when
//! `[voice].openrouter_stt` is true. That flag defaults to false, so a missing
//! local model never sends audio to the network.
//!
//! TTS defaults off. When `[voice].tts` is true, a voice-origin reply keeps
//! the text and may add an OGG/Opus copy. Typed chat is not spoken. Synthesis
//! failure leaves the text in place. The voice-mode sentence
//! ([`VOICE_MODE_PROMPT_YUE`]) is prompt text only: this library does not ask
//! the model to shell out for transcription or synthesis.
//!
//! Bot API download and voice-note ingress are not in this module. They live
//! in the Telegram adapter. Hardware adapters must not grow a copy of that
//! download path; they may call this library later for off-device ASR.
//!
//! ADR-0063: `enabled` gates the pipeline (absent means on). `languages`
//! selects from `{yue, zh, en, ja, ko}` and becomes the SenseVoice flag
//! (one id, or `auto`). Spoken replies are Cantonese or English only.
//! English weights are the local Piper pack in [`packs::EN_TTS_PACK_ID`].

mod packs;

pub use packs::{
    merge_voice_settings, PackStatus, VoiceDownloadView, VoicePack, VoicePackFetcher, VoiceRuntime,
    VoiceSettingsPatch, EN_TTS_PACK_ID, EN_TTS_PACK_URL, EN_TTS_REL_DIR, SENSEVOICE_PACK_ID,
    SENSEVOICE_PACK_URL, YUE_TTS_PACK_ID,
};

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use serde::Deserialize;
use serde::Deserializer;
use tokio::process::Command;

/// Inbound prefix for a transcribed voice note. The transcript follows on the
/// next line, with no extra label.
pub const VOICE_NOTE_PREFIX: &str = "[Voice note]:\n";

/// Spoken-text cap for one OGG/Opus reply. The text copy is never truncated.
pub const TTS_MAX_CHARS: usize = 3500;

const DEFAULT_STT_BIN: &str = "sherpa-onnx-offline";
const DEFAULT_TTS_BIN: &str = "sherpa-onnx-offline-tts";
const DEFAULT_FFMPEG_BIN: &str = "ffmpeg";
const SENSEVOICE_LANGS: &[&str] = &["auto", "zh", "en", "ja", "ko", "yue"];

/// Selection order for `[voice].languages`. `auto` is a CLI flag, not an id.
pub const VOICE_LANG_ORDER: [&str; 5] = ["yue", "zh", "en", "ja", "ko"];

/// Caption-less note when `[voice].enabled` is false. Not a silent drop.
pub const VOICE_OFF_CLARIFY: &str = "Voice notes are turned off. Please type your message.";

fn default_engine() -> String {
    "sensevoice".to_string()
}

fn default_language() -> String {
    "yue".to_string()
}

fn default_voice_enabled() -> bool {
    true
}

fn default_voice_languages() -> Vec<String> {
    vec!["yue".to_string(), "en".to_string()]
}

fn default_openrouter_stt_model() -> String {
    "openai/whisper-large-v3".to_string()
}

fn default_voice_timeout_secs() -> u64 {
    120
}

/// `[voice]` config. Absent section matches [`Default`]: SenseVoice on,
/// languages `yue` + `en` (CLI `auto`), cloud STT off, TTS off.
///
/// `language` is the legacy single-id key. It is the selection only when
/// `languages` is absent. `languages` wins once Settings (or the file) sets it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceConfig {
    /// Master gate for STT and TTS. Absent in the file means true.
    pub enabled: bool,
    /// Primary engine id. Only `sensevoice` is implemented.
    pub engine: String,
    /// Directory with `model.int8.onnx` (or `model.onnx`) and `tokens.txt`.
    /// Empty until home resolution fills `<home>/models/sensevoice`.
    pub model_dir: PathBuf,
    /// Override for `sherpa-onnx-offline`. Empty uses that name on `PATH`.
    pub stt_bin: String,
    /// Override for `ffmpeg` (OGG/Opus voice notes → 16 kHz mono WAV).
    pub ffmpeg_bin: String,
    /// SenseVoice CLI flag derived from [`VoiceConfig::languages`]: one id,
    /// or `auto` when two or more are selected.
    pub language: String,
    /// Selected SenseVoice languages, ordered `yue, zh, en, ja, ko`.
    pub languages: Vec<String>,
    /// Opt-in OpenRouter speech-to-text. Default false. Not a Settings field.
    pub openrouter_stt: bool,
    /// Model id used only when `openrouter_stt` is true.
    pub openrouter_stt_model: String,
    /// Wall clock for one local (or fallback) transcription. `0` becomes 120.
    pub stt_timeout_secs: u64,
    /// Opt-in spoken reply. Default false. Text is still sent when this is on.
    pub tts: bool,
    /// Override that writes OGG/Opus. Empty uses sherpa-onnx VITS + ffmpeg.
    /// Contract: `bin --text-file <txt> --output <file.ogg>`.
    pub tts_bin: String,
    /// Directory with `model.onnx` and `tokens.txt` for Cantonese TTS.
    /// Optional `lexicon.txt` and `rule.fst` are passed when those files exist.
    /// Empty until home resolution fills `<home>/models/tts-cantonese`.
    pub tts_model_dir: PathBuf,
    /// Directory for [`EN_TTS_PACK_ID`]. Empty until home resolution fills
    /// `<home>/models/tts-english`.
    pub tts_en_model_dir: PathBuf,
    /// Wall clock for one synthesis. `0` becomes 120.
    pub tts_timeout_secs: u64,
}

/// File shape. `languages` and legacy `language` stay optional so an absent
/// key is not the same as an empty list or the old default `yue`.
#[derive(Debug, Deserialize)]
#[serde(default)]
struct VoiceConfigFile {
    #[serde(default = "default_voice_enabled")]
    enabled: bool,
    #[serde(default = "default_engine")]
    engine: String,
    #[serde(default)]
    model_dir: PathBuf,
    #[serde(default)]
    stt_bin: String,
    #[serde(default)]
    ffmpeg_bin: String,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    languages: Option<Vec<String>>,
    #[serde(default)]
    openrouter_stt: bool,
    #[serde(default = "default_openrouter_stt_model")]
    openrouter_stt_model: String,
    #[serde(default = "default_voice_timeout_secs")]
    stt_timeout_secs: u64,
    #[serde(default)]
    tts: bool,
    #[serde(default)]
    tts_bin: String,
    #[serde(default)]
    tts_model_dir: PathBuf,
    #[serde(default)]
    tts_en_model_dir: PathBuf,
    #[serde(default = "default_voice_timeout_secs")]
    tts_timeout_secs: u64,
}

impl Default for VoiceConfigFile {
    fn default() -> Self {
        Self {
            enabled: default_voice_enabled(),
            engine: default_engine(),
            model_dir: PathBuf::new(),
            stt_bin: String::new(),
            ffmpeg_bin: String::new(),
            language: None,
            languages: None,
            openrouter_stt: false,
            openrouter_stt_model: default_openrouter_stt_model(),
            stt_timeout_secs: default_voice_timeout_secs(),
            tts: false,
            tts_bin: String::new(),
            tts_model_dir: PathBuf::new(),
            tts_en_model_dir: PathBuf::new(),
            tts_timeout_secs: default_voice_timeout_secs(),
        }
    }
}

impl<'de> Deserialize<'de> for VoiceConfig {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = VoiceConfigFile::deserialize(deserializer)?;
        raw.into_voice().map_err(serde::de::Error::custom)
    }
}

impl VoiceConfigFile {
    fn into_voice(self) -> Result<VoiceConfig, String> {
        let languages = match self.languages {
            Some(languages) => order_voice_languages(&languages)?,
            None => match self.language.as_deref() {
                Some(language) => selection_from_legacy(language),
                None => default_voice_languages(),
            },
        };
        let mut cfg = VoiceConfig {
            enabled: self.enabled,
            engine: self.engine,
            model_dir: self.model_dir,
            stt_bin: self.stt_bin,
            ffmpeg_bin: self.ffmpeg_bin,
            language: String::new(),
            languages,
            openrouter_stt: self.openrouter_stt,
            openrouter_stt_model: self.openrouter_stt_model,
            stt_timeout_secs: self.stt_timeout_secs,
            tts: self.tts,
            tts_bin: self.tts_bin,
            tts_model_dir: self.tts_model_dir,
            tts_en_model_dir: self.tts_en_model_dir,
            tts_timeout_secs: self.tts_timeout_secs,
        };
        cfg.normalize();
        Ok(cfg)
    }
}

impl Default for VoiceConfig {
    fn default() -> Self {
        let mut cfg = Self {
            enabled: true,
            engine: default_engine(),
            model_dir: PathBuf::new(),
            stt_bin: String::new(),
            ffmpeg_bin: String::new(),
            language: String::new(),
            languages: default_voice_languages(),
            openrouter_stt: false,
            openrouter_stt_model: default_openrouter_stt_model(),
            stt_timeout_secs: default_voice_timeout_secs(),
            tts: false,
            tts_bin: String::new(),
            tts_model_dir: PathBuf::new(),
            tts_en_model_dir: PathBuf::new(),
            tts_timeout_secs: default_voice_timeout_secs(),
        };
        cfg.normalize();
        cfg
    }
}

impl VoiceConfig {
    /// Fill blanks and derive the SenseVoice CLI flag from `languages`.
    pub fn normalize(&mut self) {
        if self.engine.trim().is_empty() {
            self.engine = default_engine();
        } else {
            self.engine = self.engine.trim().to_ascii_lowercase();
        }
        if self.languages.is_empty() {
            self.languages = default_voice_languages();
        }
        if let Ok(ordered) = order_voice_languages(&self.languages) {
            self.languages = ordered;
        }
        self.language = stt_language_flag(&self.languages);
        if self.openrouter_stt_model.trim().is_empty() {
            self.openrouter_stt_model = default_openrouter_stt_model();
        } else {
            self.openrouter_stt_model = self.openrouter_stt_model.trim().to_string();
        }
        if self.stt_timeout_secs == 0 {
            self.stt_timeout_secs = default_voice_timeout_secs();
        }
        if self.tts_timeout_secs == 0 {
            self.tts_timeout_secs = default_voice_timeout_secs();
        }
        self.stt_bin = self.stt_bin.trim().to_string();
        self.tts_bin = self.tts_bin.trim().to_string();
        self.ffmpeg_bin = self.ffmpeg_bin.trim().to_string();
    }

    /// `--sense-voice-language` value for this selection.
    pub fn stt_language_flag(&self) -> String {
        stt_language_flag(&self.languages)
    }
}

/// Reject an empty list or any id outside `{yue, zh, en, ja, ko}`.
/// Duplicates collapse. Stored order is [`VOICE_LANG_ORDER`].
pub fn order_voice_languages(ids: &[String]) -> Result<Vec<String>, String> {
    if ids.is_empty() {
        return Err("voice languages must not be empty (allowed: yue, zh, en, ja, ko)".to_string());
    }
    let mut chosen = [false; 5];
    for raw in ids {
        let id = raw.trim().to_ascii_lowercase();
        if id.is_empty() {
            return Err(
                "voice languages must not be empty (allowed: yue, zh, en, ja, ko)".to_string(),
            );
        }
        let Some(index) = VOICE_LANG_ORDER.iter().position(|lang| *lang == id) else {
            return Err(format!(
                "unknown voice language '{id}' (allowed: yue, zh, en, ja, ko)"
            ));
        };
        chosen[index] = true;
    }
    Ok(VOICE_LANG_ORDER
        .iter()
        .enumerate()
        .filter(|(index, _)| chosen[*index])
        .map(|(_, lang)| (*lang).to_string())
        .collect())
}

/// One selected id is that id. Two or more is `auto`.
pub fn stt_language_flag(languages: &[String]) -> String {
    if languages.len() == 1 {
        languages[0].clone()
    } else {
        "auto".to_string()
    }
}

/// Legacy `language = "yue"` (and the other ids) stays a one-id selection.
/// Legacy `auto` keeps every id so the CLI flag stays `auto`. An unknown
/// legacy value stays Cantonese, matching [`sensevoice_language`].
fn selection_from_legacy(language: &str) -> Vec<String> {
    let id = language.trim().to_ascii_lowercase();
    if id == "auto" {
        return VOICE_LANG_ORDER
            .iter()
            .map(|lang| (*lang).to_string())
            .collect();
    }
    if VOICE_LANG_ORDER.contains(&id.as_str()) {
        vec![id]
    } else {
        vec![default_language()]
    }
}

/// Where a successful transcript came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SttSource {
    SenseVoice,
    OpenRouter,
}

/// Non-empty transcript plus which engine produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transcript {
    pub text: String,
    pub source: SttSource,
    /// SenseVoice `lang` or language tag. OpenRouter leaves this empty.
    pub detected_lang: Option<String>,
}

/// Text from one engine, plus a language report when the engine has one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recognized {
    text: String,
    detected_lang: Option<String>,
}

impl Recognized {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            detected_lang: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AsrErrorKind {
    Download,
    Unavailable,
    Failed,
    Empty,
    TimedOut,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsrError {
    pub kind: AsrErrorKind,
    pub message: String,
    /// True only after an opt-in cloud attempt actually ran (or refused
    /// because the key was empty while the flag was on and local had failed).
    pub cloud_attempted: bool,
}

impl std::fmt::Display for AsrError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AsrError {}

impl AsrError {
    pub fn download(message: impl Into<String>) -> Self {
        Self {
            kind: AsrErrorKind::Download,
            message: message.into(),
            cloud_attempted: false,
        }
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self {
            kind: AsrErrorKind::Unavailable,
            message: message.into(),
            cloud_attempted: false,
        }
    }

    pub fn failed(message: impl Into<String>) -> Self {
        Self {
            kind: AsrErrorKind::Failed,
            message: message.into(),
            cloud_attempted: false,
        }
    }

    pub fn empty(message: impl Into<String>) -> Self {
        Self {
            kind: AsrErrorKind::Empty,
            message: message.into(),
            cloud_attempted: false,
        }
    }

    pub fn timed_out(message: impl Into<String>) -> Self {
        Self {
            kind: AsrErrorKind::TimedOut,
            message: message.into(),
            cloud_attempted: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TtsError {
    pub message: String,
}

impl std::fmt::Display for TtsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for TtsError {}

impl TtsError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// User-facing line when a voice note cannot enter the turn as text.
///
/// Stable phrases: download failures say "couldn't download"; cloud-off
/// failures say "cloud transcription is off"; a failed opt-in fallback names
/// "OpenRouter". The internal `AsrError` message stays in the log.
pub fn voice_note_clarify(err: &AsrError) -> String {
    if err.kind == AsrErrorKind::Download {
        return "I got your voice note but couldn't download the audio. Please try again, or type your message.".to_string();
    }
    if err.cloud_attempted {
        return "I got your voice note but couldn't transcribe it. Local SenseVoice and the OpenRouter fallback both failed. Please type your message.".to_string();
    }
    "I got your voice note but couldn't transcribe it. Local SenseVoice isn't ready, and cloud transcription is off. Please type your message.".to_string()
}

/// Product lock (#363). Spoken-reply instruction for a voice-origin turn.
/// Default Cantonese. Latin-only soul/user text uses [`VOICE_MODE_PROMPT_EN`].
pub const VOICE_MODE_PROMPT_YUE: &str =
    "你而家用語音回覆：答案要短、口語、適合直接講出嚟；唔好長篇工具報告。";

/// English adaptation of [`VOICE_MODE_PROMPT_YUE`] when soul and user files
/// have Latin letters and no CJK.
pub const VOICE_MODE_PROMPT_EN: &str = "You are replying by voice: keep the answer short, spoken, and ready to say aloud. Do not write a long tool report.";

/// Voice-mode sentence for this turn.
///
/// Empty soul and user text stay Cantonese. Any CJK ideograph in those files
/// stays Cantonese. Latin letters and no CJK adapt to English.
pub fn voice_mode_instruction(soul: &str, user_model: &str) -> &'static str {
    let has_cjk = soul.chars().chain(user_model.chars()).any(is_cjk_ideograph);
    let has_latin = soul
        .chars()
        .chain(user_model.chars())
        .any(|c| c.is_ascii_alphabetic());
    if !has_cjk && has_latin {
        VOICE_MODE_PROMPT_EN
    } else {
        VOICE_MODE_PROMPT_YUE
    }
}

/// System-prompt suffix inserted only for a voice-origin turn.
pub fn voice_mode_system_suffix(soul: &str, user_model: &str) -> String {
    format!(
        "\n\n# Voice reply\n\n{}",
        voice_mode_instruction(soul, user_model)
    )
}

fn is_cjk_ideograph(c: char) -> bool {
    matches!(c, '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}')
}

/// Exact inbound shape: `[Voice note]:` then the transcript on the next line.
pub fn shape_voice_note(transcript: &str) -> String {
    format!("{VOICE_NOTE_PREFIX}{}", transcript.trim())
}

/// SenseVoice language flag. Unknown or blank values become `yue`.
pub fn sensevoice_language(raw: &str) -> String {
    let trimmed = raw.trim().to_ascii_lowercase();
    if SENSEVOICE_LANGS.contains(&trimmed.as_str()) {
        trimmed
    } else {
        default_language()
    }
}

/// Language SenseVoice reported, before tags are stripped.
///
/// JSON `lang` wins. Otherwise the first `<|yue|>` / `<|zh|>` / `<|en|>` /
/// `<|ja|>` / `<|ko|>` tag. Emotion and event tags are ignored.
pub fn sensevoice_reported_language(raw: &str) -> Option<String> {
    if let Some(lang) = first_json_string_field(raw, "lang") {
        return canonical_voice_id(&lang);
    }
    let mut rest = raw;
    while let Some(start) = rest.find("<|") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("|>") else {
            break;
        };
        if let Some(id) = canonical_voice_id(&after[..end]) {
            return Some(id);
        }
        rest = &after[end + 2..];
    }
    None
}

fn canonical_voice_id(raw: &str) -> Option<String> {
    let id = raw.trim().to_ascii_lowercase();
    VOICE_LANG_ORDER.contains(&id.as_str()).then_some(id)
}

/// Spoken reply language. `yue` and `en` speak. `zh`, `ja`, `ko`, and an
/// `auto` result that is not `yue` or `en` stay text-only.
///
/// A single CLI id of `yue` or `en` speaks even when SenseVoice did not
/// report a language. `auto` speaks only when the report is `yue` or `en`.
pub fn spoken_reply_language(cli_flag: &str, reported: Option<&str>) -> Option<String> {
    let cli = cli_flag.trim().to_ascii_lowercase();
    if cli == "yue" || cli == "en" {
        return Some(cli);
    }
    if matches!(cli.as_str(), "zh" | "ja" | "ko") {
        return None;
    }
    match reported.map(|lang| lang.trim().to_ascii_lowercase()) {
        Some(id) if id == "yue" || id == "en" => Some(id),
        _ => None,
    }
}

/// argv for `sherpa-onnx-offline` (or `stt_bin`) excluding the program name.
pub fn sensevoice_args(tokens: &Path, model: &Path, language: &str, wav: &Path) -> Vec<String> {
    vec![
        format!("--tokens={}", tokens.display()),
        format!("--sense-voice-model={}", model.display()),
        format!("--sense-voice-language={language}"),
        "--num-threads=2".to_string(),
        "--debug=0".to_string(),
        wav.display().to_string(),
    ]
}

/// argv for ffmpeg WAV conversion, excluding the program name.
pub fn ffmpeg_to_wav_args(input: &Path, output: &Path) -> Vec<String> {
    vec![
        "-y".to_string(),
        "-i".to_string(),
        input.display().to_string(),
        "-ar".to_string(),
        "16000".to_string(),
        "-ac".to_string(),
        "1".to_string(),
        "-f".to_string(),
        "wav".to_string(),
        output.display().to_string(),
    ]
}

/// argv for ffmpeg OGG/Opus encoding, excluding the program name.
pub fn ffmpeg_ogg_opus_args(wav: &Path, ogg: &Path) -> Vec<String> {
    vec![
        "-y".to_string(),
        "-i".to_string(),
        wav.display().to_string(),
        "-c:a".to_string(),
        "libopus".to_string(),
        "-b:a".to_string(),
        "32k".to_string(),
        "-vbr".to_string(),
        "on".to_string(),
        "-application".to_string(),
        "voip".to_string(),
        ogg.display().to_string(),
    ]
}

/// argv for `sherpa-onnx-offline-tts`, excluding the program name.
/// `text` is the final positional argument.
///
/// Optional files under `tts_model_dir` are added only when they exist:
/// `lexicon.txt` (`--vits-lexicon`), `rule.fst` (`--tts-rule-fsts`), and
/// `espeak-ng-data/` (`--vits-data-dir`).
pub fn vits_tts_args(
    model: &Path,
    tokens: &Path,
    tts_model_dir: &Path,
    wav_out: &Path,
    text: &str,
) -> Vec<String> {
    let mut args = vec![
        format!("--vits-model={}", model.display()),
        format!("--vits-tokens={}", tokens.display()),
    ];
    let lexicon = tts_model_dir.join("lexicon.txt");
    if lexicon.is_file() {
        args.push(format!("--vits-lexicon={}", lexicon.display()));
    }
    let rule_fst = tts_model_dir.join("rule.fst");
    if rule_fst.is_file() {
        args.push(format!("--tts-rule-fsts={}", rule_fst.display()));
    }
    let data_dir = tts_model_dir.join("espeak-ng-data");
    if data_dir.is_dir() {
        args.push(format!("--vits-data-dir={}", data_dir.display()));
    }
    args.push(format!("--output-filename={}", wav_out.display()));
    let spoken = if text.starts_with('-') {
        format!(" {text}")
    } else {
        text.to_string()
    };
    args.push(spoken);
    args
}

/// Transcript from `sherpa-onnx-offline`, with SenseVoice tags removed.
///
/// v2.0.1 writes one JSON object per file on stdout (`"text"` is the
/// transcript) and the wav path plus `----` on stderr. Older builds put a
/// tagged line immediately before `----`. JSON `text` wins, including when
/// it is empty, so a path-only line before `----` is not the transcript.
pub fn parse_sensevoice_output(text: &str) -> String {
    if let Some(from_json) = first_json_text(text) {
        return strip_sensevoice_tags(&from_json);
    }
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let raw = if let Some(pos) = lines.iter().rposition(|line| *line == "----") {
        lines[..pos]
            .iter()
            .rev()
            .copied()
            .find(|line| !is_audio_path_line(line))
            .unwrap_or("")
    } else {
        lines
            .iter()
            .rev()
            .copied()
            .find(|line| !is_audio_path_line(line))
            .unwrap_or("")
    };
    strip_sensevoice_tags(raw)
}

/// First JSON object line that has a string `text` field.
///
/// The CLI caller joins stdout ahead of stderr, and v2.0.1 prints the
/// recognizer result as that first object. An empty string is still a hit.
fn first_json_text(blob: &str) -> Option<String> {
    first_json_string_field(blob, "text")
}

fn first_json_string_field(blob: &str, field: &str) -> Option<String> {
    blob.lines().find_map(|line| {
        let line = line.trim();
        if !line.starts_with('{') {
            return None;
        }
        let value: serde_json::Value = serde_json::from_str(line).ok()?;
        value.get(field)?.as_str().map(str::to_string)
    })
}

/// Sherpa prints the input wav path on its own stderr line. That line is
/// not speech. A sentence that merely mentions a filename is left alone.
fn is_audio_path_line(line: &str) -> bool {
    let line = line.trim();
    if line.is_empty() || line.contains('<') || line.contains('{') {
        return false;
    }
    let Some((_, ext)) = line.rsplit_once('.') else {
        return false;
    };
    let ext = ext.to_ascii_lowercase();
    let audio = matches!(
        ext.as_str(),
        "wav" | "ogg" | "mp3" | "flac" | "m4a" | "opus" | "oga" | "aac" | "webm"
    );
    if !audio {
        return false;
    }
    line.contains('/') || line.contains('\\') || !line.contains(char::is_whitespace)
}

const TRANSCRIPT_LOG_CHARS: usize = 80;

fn transcript_preview(text: &str) -> String {
    text.chars().take(TRANSCRIPT_LOG_CHARS).collect()
}

/// Drop `<|yue|>` / `<|Speech|>` style tags SenseVoice prints around the words.
pub fn strip_sensevoice_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<|") {
        out.push_str(&rest[..start]);
        if let Some(end) = rest[start..].find("|>") {
            rest = &rest[start + end + 2..];
        } else {
            out.push_str(&rest[start..]);
            rest = "";
            break;
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

/// Text actually passed to a synthesizer. The chat copy stays full length.
pub fn tts_spoken_text(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= TTS_MAX_CHARS {
        trimmed.to_string()
    } else {
        trimmed.chars().take(TTS_MAX_CHARS).collect()
    }
}

fn audio_format(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "wav" => "wav",
        "mp3" => "mp3",
        "flac" => "flac",
        "m4a" => "m4a",
        "webm" => "webm",
        "aac" => "aac",
        "ogg" | "opus" | "oga" => "ogg",
        _ => "ogg",
    }
}

/// ISO-639-1 hint for OpenRouter. Cantonese `yue` is omitted so Whisper is
/// not forced into Mandarin (`zh`).
pub fn openrouter_language_hint(sensevoice_lang: &str) -> Option<String> {
    match sensevoice_language(sensevoice_lang).as_str() {
        "en" => Some("en".to_string()),
        "ja" => Some("ja".to_string()),
        "ko" => Some("ko".to_string()),
        "zh" => Some("zh".to_string()),
        _ => None,
    }
}

fn bin_path(override_bin: &str, default_name: &str) -> PathBuf {
    if override_bin.trim().is_empty() {
        PathBuf::from(default_name)
    } else {
        PathBuf::from(override_bin.trim())
    }
}

fn resolve_sensevoice_model(model_dir: &Path) -> Result<(PathBuf, PathBuf), AsrError> {
    let tokens = model_dir.join("tokens.txt");
    let model = ["model.int8.onnx", "model.onnx"]
        .into_iter()
        .map(|name| model_dir.join(name))
        .find(|path| path.is_file());
    match model {
        Some(model) if tokens.is_file() => Ok((model, tokens)),
        _ => Err(AsrError::unavailable(format!(
            "SenseVoice model not found in {} (need model.int8.onnx or model.onnx, plus tokens.txt). \
             Download sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2025-09-09 and point voice.model_dir at that folder.",
            model_dir.display()
        ))),
    }
}

struct Captured {
    stdout: String,
    stderr: String,
}

async fn run_capturing(
    bin: &Path,
    args: &[String],
    timeout_secs: u64,
) -> Result<Captured, AsrError> {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .kill_on_drop(true)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let child = cmd.spawn().map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            AsrError::unavailable(format!("could not start {}: {err}", bin.display()))
        } else {
            AsrError::failed(format!("could not start {}: {err}", bin.display()))
        }
    })?;
    let secs = if timeout_secs == 0 {
        default_voice_timeout_secs()
    } else {
        timeout_secs
    };
    let output =
        match tokio::time::timeout(Duration::from_secs(secs), child.wait_with_output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(err)) => {
                return Err(AsrError::failed(format!(
                    "{} failed while waiting: {err}",
                    bin.display()
                )))
            }
            Err(_) => {
                return Err(AsrError::timed_out(format!(
                    "{} timed out after {secs}s",
                    bin.display()
                )))
            }
        };
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !output.status.success() {
        let detail = tail(&format!("{stderr}\n{stdout}"), 500);
        return Err(AsrError::failed(format!(
            "{} exited {}: {detail}",
            bin.display(),
            output
                .status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "signal".to_string())
        )));
    }
    Ok(Captured { stdout, stderr })
}

fn tail(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    trimmed
        .chars()
        .rev()
        .take(max)
        .collect::<String>()
        .chars()
        .rev()
        .collect()
}

fn is_wav(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("wav"))
}

/// Convert non-WAV audio to 16 kHz mono WAV. WAV inputs are returned as-is.
async fn prepare_wav(cfg: &VoiceConfig, input: &Path) -> Result<(PathBuf, bool), AsrError> {
    if is_wav(input) {
        return Ok((input.to_path_buf(), false));
    }
    let output = input.with_file_name(format!("rustfox-16k-{}.wav", uuid::Uuid::new_v4()));
    let bin = bin_path(&cfg.ffmpeg_bin, DEFAULT_FFMPEG_BIN);
    let args = ffmpeg_to_wav_args(input, &output);
    run_capturing(&bin, &args, cfg.stt_timeout_secs)
        .await
        .map_err(|err| {
            if err.kind == AsrErrorKind::Unavailable {
                AsrError::unavailable(format!(
                    "ffmpeg is required to convert Telegram voice notes to WAV ({})",
                    err.message
                ))
            } else {
                err
            }
        })?;
    if !output.is_file() {
        return Err(AsrError::failed(
            "ffmpeg exited 0 but did not write a WAV file",
        ));
    }
    Ok((output, true))
}

/// Local engine, then optional cloud. `remote == None` never calls cloud.
#[async_trait]
pub trait SpeechToText: Send + Sync {
    async fn transcribe(&self, path: &Path) -> Result<Recognized, AsrError>;
}

/// Local SenseVoice CLI. Shareable with a later off-device ASR caller.
pub struct SenseVoiceCli {
    pub cfg: VoiceConfig,
}

#[async_trait]
impl SpeechToText for SenseVoiceCli {
    async fn transcribe(&self, path: &Path) -> Result<Recognized, AsrError> {
        if !self.cfg.engine.eq_ignore_ascii_case("sensevoice") {
            return Err(AsrError::unavailable(format!(
                "voice.engine is '{}'; the primary engine is SenseVoice",
                self.cfg.engine
            )));
        }
        if !path.is_file() {
            return Err(AsrError::failed(format!(
                "audio file missing: {}",
                path.display()
            )));
        }
        let (model, tokens) = resolve_sensevoice_model(&self.cfg.model_dir)?;
        let (wav, cleanup) = prepare_wav(&self.cfg, path).await?;
        let bin = bin_path(&self.cfg.stt_bin, DEFAULT_STT_BIN);
        let language = self.cfg.stt_language_flag();
        let args = sensevoice_args(&tokens, &model, &language, &wav);
        let captured = run_capturing(&bin, &args, self.cfg.stt_timeout_secs).await;
        if cleanup {
            let _ = tokio::fs::remove_file(&wav).await;
        }
        let captured = captured?;
        let combined = format!("{}\n{}", captured.stdout, captured.stderr);
        let detected_lang = sensevoice_reported_language(&combined);
        let text = parse_sensevoice_output(&combined);
        if text.is_empty() {
            Err(AsrError::empty("SenseVoice produced no transcript"))
        } else {
            tracing::info!(
                preview = %transcript_preview(&text),
                lang = detected_lang.as_deref().unwrap_or(""),
                "SenseVoice transcript"
            );
            Ok(Recognized {
                text,
                detected_lang,
            })
        }
    }
}

/// Opt-in OpenRouter speech-to-text. Constructed only when the flag is on.
pub struct OpenRouterStt {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub language: Option<String>,
    pub timeout: Duration,
}

#[async_trait]
impl SpeechToText for OpenRouterStt {
    async fn transcribe(&self, path: &Path) -> Result<Recognized, AsrError> {
        if self.api_key.trim().is_empty() {
            return Err(AsrError {
                kind: AsrErrorKind::Unavailable,
                message: "OpenRouter STT is enabled but the API key is empty".to_string(),
                cloud_attempted: true,
            });
        }
        let bytes = tokio::fs::read(path)
            .await
            .map_err(|err| AsrError::failed(format!("read audio for OpenRouter STT: {err}")))?;
        let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
        let format = audio_format(path);
        let mut body = serde_json::json!({
            "model": self.model,
            "input_audio": {
                "data": encoded,
                "format": format,
            }
        });
        if let Some(language) = &self.language {
            body["language"] = serde_json::Value::String(language.clone());
        }
        let url = format!(
            "{}/audio/transcriptions",
            self.base_url.trim_end_matches('/')
        );
        let client = reqwest::Client::builder()
            .timeout(self.timeout)
            .build()
            .map_err(|err| AsrError::failed(format!("OpenRouter STT client: {err}")))?;
        let response = client
            .post(url)
            .bearer_auth(self.api_key.trim())
            .json(&body)
            .send()
            .await
            .map_err(|err| AsrError::failed(format!("OpenRouter STT request failed: {err}")))?;
        let status = response.status();
        let payload = response
            .text()
            .await
            .map_err(|err| AsrError::failed(format!("OpenRouter STT body: {err}")))?;
        if !status.is_success() {
            return Err(AsrError::failed(format!(
                "OpenRouter STT HTTP {status}: {}",
                tail(&payload, 300)
            )));
        }
        let parsed: serde_json::Value = serde_json::from_str(&payload)
            .map_err(|err| AsrError::failed(format!("OpenRouter STT JSON: {err}")))?;
        let text = parsed
            .get("text")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if text.is_empty() {
            Err(AsrError::empty(
                "OpenRouter STT returned an empty transcript",
            ))
        } else {
            Ok(Recognized::text(text))
        }
    }
}

pub async fn transcribe_local_first(
    local: &dyn SpeechToText,
    remote: Option<&dyn SpeechToText>,
    path: &Path,
) -> Result<Transcript, AsrError> {
    match local.transcribe(path).await {
        Ok(recognized) => {
            let text = recognized.text.trim().to_string();
            if !text.is_empty() {
                return Ok(Transcript {
                    text,
                    source: SttSource::SenseVoice,
                    detected_lang: recognized.detected_lang,
                });
            }
            fallback_or(
                AsrError::empty("local SenseVoice returned an empty transcript"),
                remote,
                path,
            )
            .await
        }
        Err(err) => fallback_or(err, remote, path).await,
    }
}

async fn fallback_or(
    local_err: AsrError,
    remote: Option<&dyn SpeechToText>,
    path: &Path,
) -> Result<Transcript, AsrError> {
    let Some(remote) = remote else {
        return Err(local_err);
    };
    tracing::warn!(
        error = %local_err,
        "local SenseVoice failed; trying opt-in OpenRouter STT"
    );
    match remote.transcribe(path).await {
        Ok(recognized) => {
            let text = recognized.text.trim().to_string();
            if text.is_empty() {
                Err(AsrError {
                    kind: AsrErrorKind::Empty,
                    message: format!(
                        "local SenseVoice failed ({local_err}); OpenRouter STT returned an empty transcript"
                    ),
                    cloud_attempted: true,
                })
            } else {
                Ok(Transcript {
                    text,
                    source: SttSource::OpenRouter,
                    detected_lang: None,
                })
            }
        }
        Err(err) => Err(AsrError {
            kind: err.kind,
            message: format!(
                "local SenseVoice failed ({local_err}); OpenRouter STT failed ({err})"
            ),
            cloud_attempted: true,
        }),
    }
}

/// Transcribe one audio file. Cloud runs only when `voice.openrouter_stt` is set.
pub async fn transcribe_file(
    voice: &VoiceConfig,
    openrouter_base: &str,
    openrouter_key: &str,
    path: &Path,
) -> Result<Transcript, AsrError> {
    if !voice.engine.eq_ignore_ascii_case("sensevoice") {
        return Err(AsrError::unavailable(format!(
            "voice.engine is '{}'; the primary engine is SenseVoice",
            voice.engine
        )));
    }
    if !path.is_file() {
        return Err(AsrError::failed(format!(
            "audio file missing: {}",
            path.display()
        )));
    }
    let local = SenseVoiceCli { cfg: voice.clone() };
    let remote = if voice.openrouter_stt {
        Some(OpenRouterStt {
            base_url: openrouter_base.to_string(),
            api_key: openrouter_key.to_string(),
            model: if voice.openrouter_stt_model.trim().is_empty() {
                default_openrouter_stt_model()
            } else {
                voice.openrouter_stt_model.trim().to_string()
            },
            language: openrouter_language_hint(&voice.stt_language_flag()),
            timeout: Duration::from_secs(voice.stt_timeout_secs.max(1)),
        })
    } else {
        None
    };
    transcribe_local_first(&local, remote.as_ref().map(|remote| remote as _), path).await
}

/// Text plus optional OGG/Opus. `text` is always the original reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyDelivery {
    pub text: String,
    pub voice_ogg: Option<Vec<u8>>,
}

#[async_trait]
pub trait TextToSpeech: Send + Sync {
    async fn synthesize_ogg_opus(&self, text: &str) -> Result<Vec<u8>, TtsError>;
}

/// When `tts_enabled` is false the synthesizer is not called.
/// When it is true and synthesis fails, `voice_ogg` is `None` and `text` remains.
pub async fn plan_reply_delivery(
    tts_enabled: bool,
    text: &str,
    synth: Option<&dyn TextToSpeech>,
) -> ReplyDelivery {
    if !tts_enabled {
        return ReplyDelivery {
            text: text.to_string(),
            voice_ogg: None,
        };
    }
    let Some(synth) = synth else {
        return ReplyDelivery {
            text: text.to_string(),
            voice_ogg: None,
        };
    };
    if text.trim().is_empty() {
        return ReplyDelivery {
            text: text.to_string(),
            voice_ogg: None,
        };
    }
    match synth.synthesize_ogg_opus(text).await {
        Ok(ogg) if !ogg.is_empty() => ReplyDelivery {
            text: text.to_string(),
            voice_ogg: Some(ogg),
        },
        Ok(_) => {
            tracing::warn!("TTS returned empty audio; text reply is kept");
            ReplyDelivery {
                text: text.to_string(),
                voice_ogg: None,
            }
        }
        Err(err) => {
            tracing::warn!(error = %err, "TTS synthesis failed; text reply is kept");
            ReplyDelivery {
                text: text.to_string(),
                voice_ogg: None,
            }
        }
    }
}

/// Local VITS TTS. `spoken_lang` picks Cantonese (`yue`) or English (`en`).
/// `tts_bin` overrides sherpa-onnx + ffmpeg for either language.
pub struct LocalTts {
    pub cfg: VoiceConfig,
    /// `Some("en")` uses [`VoiceConfig::tts_en_model_dir`]. Anything else uses
    /// the Cantonese directory.
    pub spoken_lang: Option<String>,
}

#[async_trait]
impl TextToSpeech for LocalTts {
    async fn synthesize_ogg_opus(&self, text: &str) -> Result<Vec<u8>, TtsError> {
        let spoken = tts_spoken_text(text);
        if spoken.is_empty() {
            return Err(TtsError::new("nothing to speak"));
        }
        let work = std::env::temp_dir().join(format!("rustfox-tts-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&work)
            .await
            .map_err(|err| TtsError::new(format!("tts work dir: {err}")))?;
        let result = synthesize_in(&self.cfg, self.spoken_lang.as_deref(), &work, &spoken).await;
        let _ = tokio::fs::remove_dir_all(&work).await;
        result
    }
}

async fn synthesize_in(
    cfg: &VoiceConfig,
    spoken_lang: Option<&str>,
    work: &Path,
    spoken: &str,
) -> Result<Vec<u8>, TtsError> {
    let ogg_path = work.join("reply.ogg");
    if !cfg.tts_bin.trim().is_empty() {
        let text_path = work.join("reply.txt");
        tokio::fs::write(&text_path, spoken)
            .await
            .map_err(|err| TtsError::new(format!("tts text file: {err}")))?;
        let mut cmd = Command::new(cfg.tts_bin.trim());
        cmd.arg("--text-file")
            .arg(&text_path)
            .arg("--output")
            .arg(&ogg_path)
            .kill_on_drop(true)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());
        let child = cmd
            .spawn()
            .map_err(|err| TtsError::new(format!("could not start {}: {err}", cfg.tts_bin)))?;
        let secs = cfg.tts_timeout_secs.max(1);
        let output = tokio::time::timeout(Duration::from_secs(secs), child.wait_with_output())
            .await
            .map_err(|_| TtsError::new(format!("TTS timed out after {secs}s")))?
            .map_err(|err| TtsError::new(format!("TTS wait: {err}")))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(TtsError::new(format!(
                "TTS override exited {}: {}",
                output
                    .status
                    .code()
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "signal".to_string()),
                tail(&stderr, 400)
            )));
        }
        return read_ogg(&ogg_path).await;
    }

    let wav_path = render_vits_wav(cfg, spoken_lang, work, spoken).await?;
    let ffmpeg = bin_path(&cfg.ffmpeg_bin, DEFAULT_FFMPEG_BIN);
    let ogg_args = ffmpeg_ogg_opus_args(&wav_path, &ogg_path);
    run_capturing(&ffmpeg, &ogg_args, cfg.tts_timeout_secs)
        .await
        .map_err(|err| TtsError::new(format!("ogg encode: {}", err.message)))?;
    read_ogg(&ogg_path).await
}

async fn render_vits_wav(
    cfg: &VoiceConfig,
    spoken_lang: Option<&str>,
    work: &Path,
    spoken: &str,
) -> Result<PathBuf, TtsError> {
    let english = spoken_lang == Some("en");
    let model_dir = if english {
        &cfg.tts_en_model_dir
    } else {
        &cfg.tts_model_dir
    };
    let label = if english { "English" } else { "Cantonese" };
    let model = model_dir.join("model.onnx");
    let tokens = model_dir.join("tokens.txt");
    if !model.is_file() || !tokens.is_file() {
        return Err(TtsError::new(format!(
            "{label} TTS model not found in {} (need model.onnx and tokens.txt)",
            model_dir.display()
        )));
    }
    let wav_path = work.join("reply.wav");
    let args = vits_tts_args(&model, &tokens, model_dir, &wav_path, spoken);
    let bin = bin_path("", DEFAULT_TTS_BIN);
    run_capturing(&bin, &args, cfg.tts_timeout_secs)
        .await
        .map_err(|err| TtsError::new(err.message))?;
    Ok(wav_path)
}

/// 16 kHz mono PCM s16le from the local TTS WAV stage.
///
/// The full reply is spoken. This does not encode OGG/Opus. A WAV that is
/// not already 16 kHz mono is resampled with the existing ffmpeg WAV helper.
pub async fn synthesize_pcm_s16le(
    cfg: &VoiceConfig,
    spoken_lang: &str,
    text: &str,
) -> Result<Vec<u8>, TtsError> {
    let spoken = text.trim();
    if spoken.is_empty() {
        return Err(TtsError::new("nothing to speak"));
    }
    if spoken_lang != "yue" && spoken_lang != "en" {
        return Err(TtsError::new("spoken language is not yue or en"));
    }
    let work = std::env::temp_dir().join(format!("rustfox-tts-pcm-{}", uuid::Uuid::new_v4()));
    tokio::fs::create_dir_all(&work)
        .await
        .map_err(|err| TtsError::new(format!("tts work dir: {err}")))?;
    let result = async {
        let wav_path = render_vits_wav(cfg, Some(spoken_lang), &work, spoken).await?;
        let wav = tokio::fs::read(&wav_path)
            .await
            .map_err(|err| TtsError::new(format!("read wav: {err}")))?;
        wav_bytes_to_16k_pcm(cfg, &work, &wav).await
    }
    .await;
    let _ = tokio::fs::remove_dir_all(&work).await;
    result
}

async fn wav_bytes_to_16k_pcm(
    cfg: &VoiceConfig,
    work: &Path,
    wav: &[u8],
) -> Result<Vec<u8>, TtsError> {
    if let Some(pcm) = pcm_if_16k_mono(wav) {
        return Ok(pcm);
    }
    let src = work.join("source.wav");
    let dst = work.join("speaker.wav");
    tokio::fs::write(&src, wav)
        .await
        .map_err(|err| TtsError::new(format!("tts wav file: {err}")))?;
    let ffmpeg = bin_path(&cfg.ffmpeg_bin, DEFAULT_FFMPEG_BIN);
    let args = ffmpeg_to_wav_args(&src, &dst);
    run_capturing(&ffmpeg, &args, cfg.tts_timeout_secs)
        .await
        .map_err(|err| TtsError::new(format!("pcm resample: {}", err.message)))?;
    let converted = tokio::fs::read(&dst)
        .await
        .map_err(|err| TtsError::new(format!("read resampled wav: {err}")))?;
    pcm_if_16k_mono(&converted).ok_or_else(|| TtsError::new("resampled wav is not 16 kHz mono PCM"))
}

fn pcm_if_16k_mono(wav: &[u8]) -> Option<Vec<u8>> {
    let parsed = parse_wav_pcm(wav).ok()?;
    if parsed.sample_rate == 16_000 && parsed.channels == 1 && !parsed.pcm.is_empty() {
        Some(parsed.pcm)
    } else {
        None
    }
}

/// PCM payload of a little-endian RIFF WAVE file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WavPcm {
    pub sample_rate: u32,
    pub channels: u16,
    pub pcm: Vec<u8>,
}

/// Read format-1 16-bit PCM. Unknown chunks are skipped. The `data` size is
/// the PCM length, not including a pad byte.
pub fn parse_wav_pcm(bytes: &[u8]) -> Result<WavPcm, String> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("not a RIFF WAVE file".to_string());
    }
    let mut offset = 12usize;
    let mut audio_format = None;
    let mut channels = None;
    let mut sample_rate = None;
    let mut bits = None;
    let mut pcm = None;
    while offset + 8 <= bytes.len() {
        let id = &bytes[offset..offset + 4];
        let size = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
        offset += 8;
        let end = offset
            .checked_add(size)
            .ok_or_else(|| "wav chunk size overflows".to_string())?;
        if end > bytes.len() {
            return Err("truncated wav chunk".to_string());
        }
        let chunk = &bytes[offset..end];
        if id == b"fmt " {
            if chunk.len() < 16 {
                return Err("wav fmt chunk is short".to_string());
            }
            audio_format = Some(u16::from_le_bytes(chunk[0..2].try_into().unwrap()));
            channels = Some(u16::from_le_bytes(chunk[2..4].try_into().unwrap()));
            sample_rate = Some(u32::from_le_bytes(chunk[4..8].try_into().unwrap()));
            bits = Some(u16::from_le_bytes(chunk[14..16].try_into().unwrap()));
        } else if id == b"data" && pcm.is_none() {
            pcm = Some(chunk.to_vec());
        }
        offset = end;
        if size % 2 == 1 {
            offset = offset.saturating_add(1);
        }
    }
    let (Some(format), Some(channels), Some(sample_rate), Some(bits), Some(pcm)) =
        (audio_format, channels, sample_rate, bits, pcm)
    else {
        return Err("wav is missing fmt or data".to_string());
    };
    if format != 1 {
        return Err("wav is not PCM".to_string());
    }
    if bits != 16 {
        return Err("wav is not 16-bit".to_string());
    }
    if channels == 0 {
        return Err("wav has no channels".to_string());
    }
    let frame = channels as usize * 2;
    if pcm.len() % frame != 0 {
        return Err("wav data is not a whole number of frames".to_string());
    }
    Ok(WavPcm {
        sample_rate,
        channels,
        pcm,
    })
}

async fn read_ogg(path: &Path) -> Result<Vec<u8>, TtsError> {
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|err| TtsError::new(format!("read ogg: {err}")))?;
    if bytes.is_empty() {
        return Err(TtsError::new("TTS wrote an empty ogg file"));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn asr_library_has_no_bot_api_or_device_socket() {
        let src = include_str!("mod.rs");
        let prod = src.split("mod tests").next().unwrap();
        assert!(
            !prod.contains("teloxide"),
            "ASR must not link the Telegram client"
        );
        assert!(
            !prod.contains("device_ws"),
            "ASR must not touch the device socket"
        );
        assert!(!prod.contains("get_file"));
        assert!(!prod.contains("send_voice"));
        assert!(!prod.contains("sendVoice"));
    }

    fn voice_payload_requests_shell_speech(user_text: &str, voice_mode: &str) -> bool {
        let blob = format!("{user_text}\n{voice_mode}").to_ascii_lowercase();
        [
            "execute_command",
            "sherpa-onnx",
            "whisper",
            "ffmpeg",
            ".wav",
            ".ogg",
        ]
        .iter()
        .any(|cue| blob.contains(cue))
    }

    #[test]
    fn default_voice_mode_is_the_cantonese_product_sentence() {
        let mode = voice_mode_instruction("", "");
        assert_eq!(
            mode,
            "你而家用語音回覆：答案要短、口語、適合直接講出嚟；唔好長篇工具報告。"
        );
        let suffix = voice_mode_system_suffix("  ", "\n");
        assert!(suffix.contains(mode), "{suffix}");
        assert!(suffix.contains("# Voice reply"), "{suffix}");
    }

    #[test]
    fn latin_only_soul_adapts_voice_mode_and_cjk_stays_cantonese() {
        assert_eq!(
            voice_mode_instruction("Be brief and kind.", ""),
            VOICE_MODE_PROMPT_EN
        );
        assert!(VOICE_MODE_PROMPT_EN.to_lowercase().contains("voice"));
        assert_eq!(
            voice_mode_instruction("Be brief.", "用戶想用廣東話"),
            VOICE_MODE_PROMPT_YUE
        );
        assert_eq!(
            voice_mode_instruction("", "Speak English"),
            VOICE_MODE_PROMPT_EN
        );
    }

    #[test]
    fn normal_transcript_voice_mode_does_not_ask_for_shell_speech() {
        let user = shape_voice_note("早晨");
        let mode = voice_mode_system_suffix("", "");
        assert_eq!(user, "[Voice note]:\n早晨");
        assert!(
            mode.contains("你而家用語音回覆：答案要短、口語、適合直接講出嚟；唔好長篇工具報告。"),
            "normal transcript must carry the voice-mode sentence, got {mode}"
        );
        assert!(
            !voice_payload_requests_shell_speech(&user, &mode),
            "voice-origin payload must not steer shell STT/TTS: {user}\n{mode}"
        );
        let english = voice_mode_system_suffix("Reply in English, short sentences.", "");
        assert!(
            !voice_payload_requests_shell_speech(&user, &english),
            "english voice-mode must not steer shell STT/TTS: {english}"
        );
    }

    #[test]
    fn shape_voice_note_is_exact_prefix_plus_transcript() {
        assert_eq!(shape_voice_note("  早晨  "), "[Voice note]:\n早晨");
        assert_eq!(
            shape_voice_note("hello world"),
            "[Voice note]:\nhello world"
        );
    }

    #[test]
    fn voice_defaults_keep_cloud_stt_and_tts_off() {
        let cfg = VoiceConfig::default();
        assert!(cfg.enabled);
        assert_eq!(cfg.engine, "sensevoice");
        assert_eq!(cfg.languages, vec!["yue".to_string(), "en".to_string()]);
        assert_eq!(cfg.stt_language_flag(), "auto");
        assert!(!cfg.openrouter_stt);
        assert!(!cfg.tts);
        assert_eq!(cfg.openrouter_stt_model, "openai/whisper-large-v3");

        let parsed: VoiceConfig = toml::from_str("").unwrap();
        assert!(parsed.enabled);
        assert!(!parsed.openrouter_stt);
        assert!(!parsed.tts);
        assert_eq!(parsed.engine, "sensevoice");
        assert_eq!(parsed.languages, vec!["yue".to_string(), "en".to_string()]);
        assert_eq!(parsed.stt_language_flag(), "auto");

        let partial: VoiceConfig = toml::from_str("tts = true\n").unwrap();
        assert!(partial.tts);
        assert!(partial.enabled);
        assert!(!partial.openrouter_stt);
        assert_eq!(partial.languages, vec!["yue".to_string(), "en".to_string()]);
    }

    #[test]
    fn legacy_language_is_one_id_until_languages_are_written() {
        let yue: VoiceConfig = toml::from_str("language = \"yue\"\n").unwrap();
        assert_eq!(yue.languages, vec!["yue".to_string()]);
        assert_eq!(yue.stt_language_flag(), "yue");
        assert!(yue.enabled);

        let ja: VoiceConfig = toml::from_str("language = \"JA\"\n").unwrap();
        assert_eq!(ja.languages, vec!["ja".to_string()]);
        assert_eq!(ja.stt_language_flag(), "ja");

        let auto: VoiceConfig = toml::from_str("language = \"auto\"\n").unwrap();
        assert_eq!(auto.languages.len(), 5);
        assert_eq!(auto.stt_language_flag(), "auto");
    }

    #[test]
    fn languages_keep_canonical_order_and_reject_empty_or_unknown() {
        let cfg: VoiceConfig =
            toml::from_str("languages = [\"ko\", \"yue\", \"en\", \"yue\"]\nlanguage = \"zh\"\n")
                .unwrap();
        assert_eq!(
            cfg.languages,
            vec!["yue".to_string(), "en".to_string(), "ko".to_string()]
        );
        assert_eq!(cfg.stt_language_flag(), "auto");
        assert_eq!(stt_language_flag(&["zh".to_string()]), "zh");

        assert!(toml::from_str::<VoiceConfig>("languages = []\n").is_err());
        assert!(toml::from_str::<VoiceConfig>("languages = [\"fr\"]\n").is_err());
        assert!(toml::from_str::<VoiceConfig>("languages = [\"\"]\n").is_err());

        let off: VoiceConfig = toml::from_str("enabled = false\n").unwrap();
        assert!(!off.enabled);
        assert_eq!(off.languages, vec!["yue".to_string(), "en".to_string()]);
    }

    #[test]
    fn spoken_reply_is_yue_or_en_and_other_languages_stay_text() {
        assert_eq!(spoken_reply_language("yue", None).as_deref(), Some("yue"));
        assert_eq!(
            spoken_reply_language("en", Some("zh")).as_deref(),
            Some("en")
        );
        assert_eq!(spoken_reply_language("zh", Some("en")), None);
        assert_eq!(spoken_reply_language("ja", None), None);
        assert_eq!(spoken_reply_language("ko", Some("en")), None);
        assert_eq!(
            spoken_reply_language("auto", Some("yue")).as_deref(),
            Some("yue")
        );
        assert_eq!(
            spoken_reply_language("auto", Some("EN")).as_deref(),
            Some("en")
        );
        assert_eq!(spoken_reply_language("auto", Some("zh")), None);
        assert_eq!(spoken_reply_language("auto", Some("ja")), None);
        assert_eq!(spoken_reply_language("auto", None), None);
    }

    #[test]
    fn sensevoice_report_prefers_json_lang_then_a_language_tag() {
        assert_eq!(
            sensevoice_reported_language(r#"{"lang":"en","text":"<|zh|>hi"}"#).as_deref(),
            Some("en")
        );
        assert_eq!(
            sensevoice_reported_language("<|yue|><|Speech|>早晨").as_deref(),
            Some("yue")
        );
        assert_eq!(
            sensevoice_reported_language("<|NEUTRAL|><|Speech|>hi"),
            None
        );
        assert_eq!(sensevoice_reported_language("hello"), None);
    }

    #[test]
    fn sensevoice_args_pin_cantonese_and_model() {
        let args = sensevoice_args(
            Path::new("/models/tokens.txt"),
            Path::new("/models/model.int8.onnx"),
            "yue",
            Path::new("/tmp/note.wav"),
        );
        assert!(args.iter().any(|arg| arg == "--sense-voice-language=yue"));
        assert!(args
            .iter()
            .any(|arg| arg == "--sense-voice-model=/models/model.int8.onnx"));
        assert_eq!(args.last().map(String::as_str), Some("/tmp/note.wav"));
    }

    #[test]
    fn ffmpeg_ogg_args_use_libopus() {
        let args = ffmpeg_ogg_opus_args(Path::new("in.wav"), Path::new("out.ogg"));
        assert!(args.iter().any(|arg| arg == "libopus"));
        assert_eq!(args.last().map(String::as_str), Some("out.ogg"));
    }

    #[test]
    fn vits_tts_args_pass_rule_fsts_only_when_rule_file_exists() {
        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join("model.onnx");
        let tokens = dir.path().join("tokens.txt");
        let wav = dir.path().join("out.wav");

        let absent = vits_tts_args(&model, &tokens, dir.path(), &wav, "hello");
        assert!(
            absent.iter().all(|arg| !arg.starts_with("--tts-rule-fsts")),
            "missing rule.fst must omit --tts-rule-fsts, got {absent:?}"
        );
        assert!(
            absent.iter().all(|arg| !arg.starts_with("--vits-lexicon")),
            "missing lexicon.txt must omit --vits-lexicon, got {absent:?}"
        );

        let rule = dir.path().join("rule.fst");
        std::fs::write(&rule, b"fst").unwrap();
        let present = vits_tts_args(&model, &tokens, dir.path(), &wav, "hello");
        let flag = format!("--tts-rule-fsts={}", rule.display());
        assert!(
            present.iter().any(|arg| arg == &flag),
            "expected {flag} in {present:?}"
        );

        let lexicon = dir.path().join("lexicon.txt");
        std::fs::write(&lexicon, "你 nei\n").unwrap();
        let with_lexicon = vits_tts_args(&model, &tokens, dir.path(), &wav, "hello");
        let lexicon_flag = format!("--vits-lexicon={}", lexicon.display());
        assert!(
            with_lexicon.iter().any(|arg| arg == &lexicon_flag),
            "expected {lexicon_flag} in {with_lexicon:?}"
        );
        assert!(
            with_lexicon.iter().any(|arg| arg == &flag),
            "rule.fst must stay set after lexicon.txt is added, got {with_lexicon:?}"
        );
    }

    #[test]
    fn parse_sensevoice_output_strips_tags_and_separator() {
        let raw = "\
/tmp/note.wav
<|yue|><|NEUTRAL|><|Speech|>早晨呀
----
";
        assert_eq!(parse_sensevoice_output(raw), "早晨呀");
        assert_eq!(parse_sensevoice_output("  兩隻小企鵝  \n"), "兩隻小企鵝");
    }

    /// Live `sherpa-onnx` v2.0.1 (`sherpa-onnx-offline.cc`): one
    /// `AsJsonString()` object on stdout, and the wav path immediately
    /// before `----` on stderr. The caller joins stdout then stderr.
    #[test]
    fn parse_sensevoice_output_prefers_v2_json_text_over_wav_path() {
        let stdout = concat!(
            r#"{"lang": "yue", "emotion": "NEUTRAL", "event": "Speech", "text": "早晨呀", "#,
            r#""timestamps": [0.18], "durations": [0.24], "tokens":["早", "晨", "呀"], "#,
            r#""ys_log_probs": [], "words": []}"#,
        );
        let stderr = "\
Started
Done!

/tmp/rustfox-16k-11111111-2222-3333-4444-555555555555.wav
----
num threads: 2
decoding method: greedy_search
Elapsed seconds: 0.210 s
";
        let combined = format!("{stdout}\n{stderr}");
        assert_eq!(parse_sensevoice_output(&combined), "早晨呀");
        assert!(
            !parse_sensevoice_output(&combined).contains("/tmp/"),
            "wav path must not become the transcript"
        );
    }

    #[test]
    fn parse_sensevoice_output_ignores_path_only_line_before_separator() {
        let raw = "\
/tmp/rustfox-16k-11111111-2222-3333-4444-555555555555.wav
----
num threads: 2
";
        assert_eq!(parse_sensevoice_output(raw), "");
        assert_eq!(
            parse_sensevoice_output("/tmp/note.wav\n----\n"),
            "",
            "a wav path before ---- is not a transcript"
        );
        assert_eq!(
            parse_sensevoice_output("/tmp/my note.wav\n----\n"),
            "",
            "a path with spaces before ---- is not a transcript"
        );
        assert_eq!(
            parse_sensevoice_output("please save note.wav\n----\n"),
            "please save note.wav"
        );
    }

    #[test]
    fn parse_sensevoice_output_unescapes_json_text() {
        let stdout = r#"{"lang": "en", "emotion": "", "event": "", "text": "say \"hi\"", "timestamps": [], "tokens":["say", "\"", "hi"], "words": []}"#;
        let stderr = "/tmp/note.wav\n----\n";
        assert_eq!(
            parse_sensevoice_output(&format!("{stdout}\n{stderr}")),
            "say \"hi\""
        );
    }

    #[test]
    fn parse_sensevoice_output_empty_json_text_does_not_fall_back_to_wav_path() {
        let raw = "{\"text\": \"\"}\n/tmp/note.wav\n----\n";
        assert_eq!(parse_sensevoice_output(raw), "");
    }

    #[test]
    fn transcript_preview_keeps_the_first_80_chars() {
        let text = "字".repeat(100);
        let preview = transcript_preview(&text);
        assert_eq!(preview.chars().count(), TRANSCRIPT_LOG_CHARS);
        assert!(text.starts_with(&preview));
        assert_eq!(transcript_preview("早晨呀"), "早晨呀");
    }

    #[test]
    fn parse_sensevoice_output_strips_tags_inside_json_text() {
        let stdout = r#"{"text": "<|yue|><|Speech|>兩隻小企鵝", "timestamps": []}"#;
        let stderr = "/tmp/note.wav\n----\n";
        assert_eq!(
            parse_sensevoice_output(&format!("{stdout}\n{stderr}")),
            "兩隻小企鵝"
        );
    }

    #[test]
    fn yue_is_not_sent_to_openrouter_as_mandarin() {
        assert_eq!(openrouter_language_hint("yue"), None);
        assert_eq!(openrouter_language_hint("YUE"), None);
        assert_eq!(openrouter_language_hint("en").as_deref(), Some("en"));
        assert_eq!(sensevoice_language("nope"), "yue");
    }

    #[test]
    fn clarify_says_cloud_is_off_unless_fallback_ran() {
        let local = AsrError::unavailable("model missing");
        let text = voice_note_clarify(&local);
        assert!(text.contains("voice note"));
        assert!(text.contains("cloud transcription is off"));
        assert!(text.contains("type your message"));
        assert!(!text.contains("model missing"));

        let mut cloud = AsrError::failed("http 500");
        cloud.cloud_attempted = true;
        let text = voice_note_clarify(&cloud);
        assert!(text.contains("OpenRouter"));
        assert!(!text.contains("cloud transcription is off"));

        let download = voice_note_clarify(&AsrError::download("404"));
        assert!(download.contains("couldn't download"));
        assert!(!download.contains("404"));
    }

    #[test]
    fn tts_spoken_text_caps_audio_only() {
        let long = "字".repeat(TTS_MAX_CHARS + 10);
        assert_eq!(tts_spoken_text(&long).chars().count(), TTS_MAX_CHARS);
        assert_eq!(tts_spoken_text("  早晨  "), "早晨");
    }

    struct Scripted {
        calls: Arc<AtomicUsize>,
        result: Result<Recognized, AsrError>,
    }

    #[async_trait]
    impl SpeechToText for Scripted {
        async fn transcribe(&self, _path: &Path) -> Result<Recognized, AsrError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.result.clone()
        }
    }

    #[tokio::test]
    async fn fallback_stays_off_when_remote_is_absent() {
        let local_calls = Arc::new(AtomicUsize::new(0));
        let local = Scripted {
            calls: Arc::clone(&local_calls),
            result: Err(AsrError::unavailable("no model")),
        };
        let err = transcribe_local_first(&local, None, Path::new("note.ogg"))
            .await
            .unwrap_err();
        assert_eq!(local_calls.load(Ordering::SeqCst), 1);
        assert!(!err.cloud_attempted);
        assert_eq!(err.kind, AsrErrorKind::Unavailable);
    }

    #[tokio::test]
    async fn local_success_does_not_call_remote_even_if_present() {
        let remote_calls = Arc::new(AtomicUsize::new(0));
        let local = Scripted {
            calls: Arc::new(AtomicUsize::new(0)),
            result: Ok(Recognized::text("  早晨  ")),
        };
        let remote = Scripted {
            calls: Arc::clone(&remote_calls),
            result: Ok(Recognized::text("should-not-run")),
        };
        let transcript = transcribe_local_first(&local, Some(&remote), Path::new("note.wav"))
            .await
            .unwrap();
        assert_eq!(transcript.text, "早晨");
        assert_eq!(transcript.source, SttSource::SenseVoice);
        assert_eq!(remote_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn remote_runs_only_when_passed_and_local_fails() {
        let remote_calls = Arc::new(AtomicUsize::new(0));
        let local = Scripted {
            calls: Arc::new(AtomicUsize::new(0)),
            result: Err(AsrError::failed("boom")),
        };
        let remote = Scripted {
            calls: Arc::clone(&remote_calls),
            result: Ok(Recognized::text("雲端")),
        };
        let transcript = transcribe_local_first(&local, Some(&remote), Path::new("note.ogg"))
            .await
            .unwrap();
        assert_eq!(transcript.text, "雲端");
        assert_eq!(transcript.source, SttSource::OpenRouter);
        assert_eq!(remote_calls.load(Ordering::SeqCst), 1);
    }

    struct CountingSynth {
        calls: Arc<AtomicUsize>,
        result: Result<Vec<u8>, TtsError>,
    }

    #[async_trait]
    impl TextToSpeech for CountingSynth {
        async fn synthesize_ogg_opus(&self, _text: &str) -> Result<Vec<u8>, TtsError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.result.clone()
        }
    }

    #[tokio::test]
    async fn tts_off_does_not_call_synth_and_keeps_text() {
        let calls = Arc::new(AtomicUsize::new(0));
        let synth = CountingSynth {
            calls: Arc::clone(&calls),
            result: Err(TtsError::new("must not run")),
        };
        let plan = plan_reply_delivery(false, "hello", Some(&synth)).await;
        assert_eq!(plan.text, "hello");
        assert!(plan.voice_ogg.is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn tts_on_keeps_text_and_attaches_ogg() {
        let synth = CountingSynth {
            calls: Arc::new(AtomicUsize::new(0)),
            result: Ok(b"OggS-voice".to_vec()),
        };
        let plan = plan_reply_delivery(true, "早晨", Some(&synth)).await;
        assert_eq!(plan.text, "早晨");
        assert_eq!(plan.voice_ogg.as_deref(), Some(&b"OggS-voice"[..]));
    }

    #[tokio::test]
    async fn tts_on_keeps_text_when_synth_fails() {
        let synth = CountingSynth {
            calls: Arc::new(AtomicUsize::new(0)),
            result: Err(TtsError::new("no model")),
        };
        let plan = plan_reply_delivery(true, "still here", Some(&synth)).await;
        assert_eq!(plan.text, "still here");
        assert!(plan.voice_ogg.is_none());
    }

    #[tokio::test]
    async fn openrouter_flag_off_does_not_hit_the_network() {
        let server = wiremock::MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("note.ogg");
        std::fs::write(&audio, b"OggSfake").unwrap();
        let voice = VoiceConfig {
            model_dir: PathBuf::from("/no/such/sensevoice-model-dir"),
            openrouter_stt: false,
            ..VoiceConfig::default()
        };
        let base = format!("{}/api/v1", server.uri());
        let err = transcribe_file(&voice, &base, "sk-test", &audio)
            .await
            .unwrap_err();
        assert!(!err.cloud_attempted);
        assert!(
            server.received_requests().await.unwrap().is_empty(),
            "default config must not call OpenRouter STT"
        );
        let clarify = voice_note_clarify(&err);
        assert!(clarify.contains("cloud transcription is off"));
    }

    #[tokio::test]
    async fn openrouter_flag_on_posts_transcription_after_local_failure() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, ResponseTemplate};

        let server = wiremock::MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/audio/transcriptions"))
            .and(header("authorization", "Bearer sk-test"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "text": "雲端轉錄"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("note.ogg");
        std::fs::write(&audio, b"OggSfake").unwrap();
        let voice = VoiceConfig {
            model_dir: dir.path().join("missing-model"),
            openrouter_stt: true,
            language: "yue".to_string(),
            ..VoiceConfig::default()
        };
        let base = format!("{}/api/v1", server.uri());
        let transcript = transcribe_file(&voice, &base, "sk-test", &audio)
            .await
            .unwrap();
        assert_eq!(transcript.text, "雲端轉錄");
        assert_eq!(transcript.source, SttSource::OpenRouter);
        server.verify().await;

        let reqs = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
        assert_eq!(body["model"], "openai/whisper-large-v3");
        assert!(body.get("language").is_none());
        assert_eq!(body["input_audio"]["format"], "ogg");
        assert!(body["input_audio"]["data"].as_str().unwrap().len() > 10);
    }

    #[tokio::test]
    async fn wrong_engine_does_not_fall_through_to_cloud() {
        let server = wiremock::MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("note.wav");
        std::fs::write(&audio, b"RIFFfake").unwrap();
        let voice = VoiceConfig {
            engine: "whisper.cpp".to_string(),
            openrouter_stt: true,
            ..VoiceConfig::default()
        };
        let base = format!("{}/api/v1", server.uri());
        let err = transcribe_file(&voice, &base, "sk-test", &audio)
            .await
            .unwrap_err();
        assert_eq!(err.kind, AsrErrorKind::Unavailable);
        assert!(!err.cloud_attempted);
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn enabled_tts_without_a_model_still_returns_the_text() {
        let cfg = VoiceConfig {
            tts: true,
            tts_model_dir: PathBuf::from("/no/such/tts-model"),
            ..VoiceConfig::default()
        };
        let synth = LocalTts {
            cfg,
            spoken_lang: Some("yue".to_string()),
        };
        let plan = plan_reply_delivery(true, "hello", Some(&synth)).await;
        assert_eq!(plan.text, "hello");
        assert!(plan.voice_ogg.is_none());
    }

    #[tokio::test]
    async fn english_tts_looks_in_the_en_pack_dir_and_keeps_text_when_missing() {
        let cfg = VoiceConfig {
            tts: true,
            tts_model_dir: PathBuf::from("/no/such/tts-cantonese"),
            tts_en_model_dir: PathBuf::from("/no/such/tts-english"),
            ..VoiceConfig::default()
        };
        let synth = LocalTts {
            cfg,
            spoken_lang: Some("en".to_string()),
        };
        let err = synth.synthesize_ogg_opus("hello").await.unwrap_err();
        assert!(err.to_string().contains("English"), "{err}");
        assert!(err.to_string().contains("tts-english"), "{err}");
        assert!(!err.to_string().contains("tts-cantonese"), "{err}");
        let plan = plan_reply_delivery(true, "hello", Some(&synth)).await;
        assert_eq!(plan.text, "hello");
        assert!(plan.voice_ogg.is_none());
    }

    #[cfg(unix)]
    fn write_exec(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn local_sensevoice_script_wins_over_enabled_openrouter() {
        let server = wiremock::MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join("model");
        std::fs::create_dir_all(&model).unwrap();
        std::fs::write(model.join("model.int8.onnx"), b"not-a-real-model").unwrap();
        std::fs::write(model.join("tokens.txt"), b"<blank>\n").unwrap();
        let script = write_exec(
            dir.path(),
            "sensevoice.sh",
            "#!/bin/sh\nprintf '%s\\n' '<|yue|><|Speech|>早晨'\n",
        );
        let audio = dir.path().join("note.wav");
        std::fs::write(&audio, b"RIFFfake").unwrap();

        let voice = VoiceConfig {
            model_dir: model,
            stt_bin: script.display().to_string(),
            openrouter_stt: true,
            ..VoiceConfig::default()
        };
        let base = format!("{}/api/v1", server.uri());
        let transcript = transcribe_file(&voice, &base, "sk-test", &audio)
            .await
            .unwrap();
        assert_eq!(transcript.text, "早晨");
        assert_eq!(transcript.source, SttSource::SenseVoice);
        assert_eq!(shape_voice_note(&transcript.text), "[Voice note]:\n早晨");
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn v2_sensevoice_script_returns_json_text_not_wav_path() {
        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join("model");
        std::fs::create_dir_all(&model).unwrap();
        std::fs::write(model.join("model.int8.onnx"), b"not-a-real-model").unwrap();
        std::fs::write(model.join("tokens.txt"), b"<blank>\n").unwrap();
        let script = write_exec(
            dir.path(),
            "sensevoice-v2.sh",
            r#"#!/bin/sh
printf '%s\n' '{"lang": "yue", "emotion": "NEUTRAL", "event": "Speech", "text": "早晨呀", "timestamps": [0.18], "tokens":["早", "晨", "呀"], "words": []}'
printf '%s\n' '/tmp/rustfox-16k-11111111-2222-3333-4444-555555555555.wav' '----' 'num threads: 2' >&2
"#,
        );
        let audio = dir.path().join("note.wav");
        std::fs::write(&audio, b"RIFFfake").unwrap();
        let voice = VoiceConfig {
            model_dir: model,
            stt_bin: script.display().to_string(),
            ..VoiceConfig::default()
        };
        let transcript = transcribe_file(&voice, "http://127.0.0.1:9", "", &audio)
            .await
            .unwrap();
        assert_eq!(transcript.text, "早晨呀");
        assert!(!transcript.text.contains("/tmp/"));
        assert_eq!(transcript.detected_lang.as_deref(), Some("yue"));
        assert_eq!(shape_voice_note(&transcript.text), "[Voice note]:\n早晨呀");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn sensevoice_cli_flag_is_auto_for_two_languages_and_the_id_for_one() {
        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join("model");
        std::fs::create_dir_all(&model).unwrap();
        std::fs::write(model.join("model.int8.onnx"), b"x").unwrap();
        std::fs::write(model.join("tokens.txt"), b"a\n").unwrap();
        let script = write_exec(
            dir.path(),
            "sensevoice-args.sh",
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$(dirname \"$0\")/args.txt\"\nprintf '%s\\n' '{\"text\":\"hi\",\"lang\":\"en\"}'\n",
        );
        let audio = dir.path().join("note.wav");
        std::fs::write(&audio, b"RIFFfake").unwrap();

        let voice = VoiceConfig {
            model_dir: model.clone(),
            stt_bin: script.display().to_string(),
            ..VoiceConfig::default()
        };
        assert_eq!(voice.stt_language_flag(), "auto");
        let transcript = transcribe_file(&voice, "http://127.0.0.1:9", "", &audio)
            .await
            .unwrap();
        let args = std::fs::read_to_string(dir.path().join("args.txt")).unwrap();
        assert!(
            args.lines()
                .any(|line| line == "--sense-voice-language=auto"),
            "{args}"
        );
        assert_eq!(transcript.detected_lang.as_deref(), Some("en"));
        assert_eq!(
            spoken_reply_language(
                &voice.stt_language_flag(),
                transcript.detected_lang.as_deref()
            )
            .as_deref(),
            Some("en")
        );

        let ja = VoiceConfig {
            languages: vec!["ja".to_string()],
            model_dir: model,
            stt_bin: script.display().to_string(),
            ..VoiceConfig::default()
        };
        assert_eq!(ja.stt_language_flag(), "ja");
        transcribe_file(&ja, "http://127.0.0.1:9", "", &audio)
            .await
            .unwrap();
        let args = std::fs::read_to_string(dir.path().join("args.txt")).unwrap();
        assert!(
            args.lines().any(|line| line == "--sense-voice-language=ja"),
            "{args}"
        );
        assert_eq!(spoken_reply_language("ja", Some("en")), None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn tts_bin_override_returns_ogg_and_the_same_text() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_exec(
            dir.path(),
            "tts.sh",
            "#!/bin/sh\ntext=''\nout=''\nwhile [ $# -gt 0 ]; do\n  if [ \"$1\" = \"--text-file\" ]; then text=\"$2\"; shift 2\n  elif [ \"$1\" = \"--output\" ]; then out=\"$2\"; shift 2\n  else shift; fi\ndone\nprintf 'OggS' > \"$out\"\ncat \"$text\" >> \"$out\"\n",
        );
        let cfg = VoiceConfig {
            tts: true,
            tts_bin: script.display().to_string(),
            ..VoiceConfig::default()
        };
        let synth = LocalTts {
            cfg,
            spoken_lang: Some("yue".to_string()),
        };
        let plan = plan_reply_delivery(true, "早晨", Some(&synth)).await;
        assert_eq!(plan.text, "早晨");
        let ogg = plan.voice_ogg.expect("ogg");
        assert!(ogg.starts_with(b"OggS"));
        assert!(ogg
            .windows("早晨".len())
            .any(|window| window == "早晨".as_bytes()));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn hung_stt_binary_times_out() {
        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join("model");
        std::fs::create_dir_all(&model).unwrap();
        std::fs::write(model.join("model.onnx"), b"x").unwrap();
        std::fs::write(model.join("tokens.txt"), b"a\n").unwrap();
        let script = write_exec(dir.path(), "sleep.sh", "#!/bin/sh\nexec sleep 30\n");
        let audio = dir.path().join("note.wav");
        std::fs::write(&audio, b"RIFFfake").unwrap();
        let voice = VoiceConfig {
            model_dir: model,
            stt_bin: script.display().to_string(),
            stt_timeout_secs: 1,
            ..VoiceConfig::default()
        };
        let started = std::time::Instant::now();
        let err = transcribe_file(&voice, "http://127.0.0.1:9", "", &audio)
            .await
            .unwrap_err();
        assert_eq!(err.kind, AsrErrorKind::TimedOut);
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "timeout took {:?}",
            started.elapsed()
        );
    }

    fn sample_wav(sample_rate: u32, channels: u16, frames: &[i16]) -> Vec<u8> {
        let block = channels as usize * 2;
        let data_len = frames.len() * 2;
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_len as u32).to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&sample_rate.to_le_bytes());
        out.extend_from_slice(&(sample_rate * block as u32).to_le_bytes());
        out.extend_from_slice(&(block as u16).to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data_len as u32).to_le_bytes());
        for sample in frames {
            out.extend_from_slice(&sample.to_le_bytes());
        }
        out
    }

    #[test]
    fn parse_wav_pcm_reads_16k_mono_and_skips_unknown_chunks() {
        let mut bytes = sample_wav(16_000, 1, &[1, -2, 3]);
        let mut with_junk = bytes[..12].to_vec();
        with_junk.extend_from_slice(b"JUNK");
        with_junk.extend_from_slice(&4u32.to_le_bytes());
        with_junk.extend_from_slice(b"abcd");
        with_junk.extend_from_slice(&bytes[12..]);
        bytes = with_junk;
        let parsed = parse_wav_pcm(&bytes).unwrap();
        assert_eq!(parsed.sample_rate, 16_000);
        assert_eq!(parsed.channels, 1);
        assert_eq!(
            parsed.pcm,
            1i16.to_le_bytes()
                .into_iter()
                .chain((-2i16).to_le_bytes())
                .chain(3i16.to_le_bytes())
                .collect::<Vec<_>>()
        );
        assert!(parse_wav_pcm(b"not-wav").is_err());
        assert_ne!(
            parse_wav_pcm(&sample_wav(8_000, 1, &[0]))
                .unwrap()
                .sample_rate,
            16_000
        );
        assert_eq!(
            parse_wav_pcm(&sample_wav(16_000, 2, &[0, 0]))
                .unwrap()
                .channels,
            2
        );
    }

    #[tokio::test]
    async fn pcm_synth_rejects_other_languages_and_a_missing_model() {
        let cfg = VoiceConfig::default();
        let err = synthesize_pcm_s16le(&cfg, "zh", "hello").await.unwrap_err();
        assert!(err.to_string().contains("yue") || err.to_string().contains("en"));
        let err = synthesize_pcm_s16le(&cfg, "en", "hello").await.unwrap_err();
        assert!(err.to_string().contains("model"), "{err}");
        let long = "a".repeat(TTS_MAX_CHARS + 20);
        let err = synthesize_pcm_s16le(&cfg, "yue", &long).await.unwrap_err();
        assert!(
            !err.to_string().contains("3500"),
            "a long reply is not rejected for length: {err}"
        );
    }
}
