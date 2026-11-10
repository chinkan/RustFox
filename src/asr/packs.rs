//! Voice pack download and the live settings handle (ADR-0063).
//!
//! Portal Settings publishes a new [`VoiceConfig`] here. Telegram reads the
//! same handle on the next note. A missing or failed pack is status `error`
//! or `absent` and does not fail the save or stop the process.
//!
//! English speech uses the local sherpa Piper pack
//! [`EN_TTS_PACK_ID`] at `<home>/`[`EN_TTS_REL_DIR`]
//! ([`EN_TTS_PACK_URL`]). Same offline `sherpa-onnx-offline-tts` path as
//! Cantonese [`YUE_TTS_PACK_ID`]. Not a cloud voice.

use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::AsyncWriteExt;

use super::{order_voice_languages, VoiceConfig};

/// Multi-lang SenseVoice weights already pinned by ADR-0058. One pack covers
/// `yue`, `zh`, `en`, `ja`, and `ko`.
pub const SENSEVOICE_PACK_ID: &str = "sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2025-09-09";
pub const SENSEVOICE_PACK_URL: &str =
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2025-09-09.tar.bz2";

/// Cantonese VITS pack (ADR-0058). Files land in `<home>/models/tts-cantonese`.
pub const YUE_TTS_PACK_ID: &str = "vits-cantonese-hf-xiaomaiiwn";
const YUE_TTS_BASE: &str =
    "https://huggingface.co/csukuangfj/vits-cantonese-hf-xiaomaiiwn/resolve/main";
const YUE_TTS_FILES: &[&str] = &[
    "vits-cantonese-hf-xiaomaiiwn.onnx",
    "tokens.txt",
    "lexicon.txt",
    "rule.fst",
];

/// Local English TTS (Product amend on #385).
///
/// Piper `en_US` lessac medium from the sherpa-onnx TTS models. Unpacked
/// under `<home>/models/tts-english`. The `.onnx` weights are linked to
/// `model.onnx`. `espeak-ng-data/` is passed as `--vits-data-dir` when the
/// directory is present. This is not a cloud TTS service.
pub const EN_TTS_PACK_ID: &str = "vits-piper-en_US-lessac-medium";
pub const EN_TTS_PACK_URL: &str =
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/vits-piper-en_US-lessac-medium.tar.bz2";
/// Path under the fox home when `tts_en_model_dir` is unset.
pub const EN_TTS_REL_DIR: &str = "models/tts-english";

const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);

/// Which on-disk pack a save is ensuring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VoicePack {
    SenseVoice,
    TtsCantonese,
    TtsEnglish,
}

impl VoicePack {
    pub fn id(self) -> &'static str {
        match self {
            Self::SenseVoice => SENSEVOICE_PACK_ID,
            Self::TtsCantonese => YUE_TTS_PACK_ID,
            Self::TtsEnglish => EN_TTS_PACK_ID,
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::SenseVoice => "sensevoice",
            Self::TtsCantonese => "tts-cantonese",
            Self::TtsEnglish => "tts-english",
        }
    }
}

/// Downloads one pack into `dest`. Production uses HTTPS. Tests substitute
/// a fake that never touches the network.
#[async_trait]
pub trait VoicePackFetcher: Send + Sync {
    async fn fetch_pack(&self, pack: VoicePack, dest: &Path) -> Result<(), String>;
}

/// Fields a Settings PATCH may change. `None` keeps the live value.
#[derive(Debug, Default, Clone)]
pub struct VoiceSettingsPatch {
    pub enabled: Option<bool>,
    pub languages: Option<Vec<String>>,
    pub tts: Option<bool>,
}

/// Merge a whitelist patch onto the live voice config.
///
/// An empty list or any id outside `{yue, zh, en, ja, ko}` is an error and
/// the caller writes nothing. Duplicates collapse. Order is
/// `yue, zh, en, ja, ko`.
pub fn merge_voice_settings(
    current: &VoiceConfig,
    patch: &VoiceSettingsPatch,
) -> Result<VoiceConfig, String> {
    let mut next = current.clone();
    if let Some(enabled) = patch.enabled {
        next.enabled = enabled;
    }
    if let Some(languages) = &patch.languages {
        next.languages = order_voice_languages(languages)?;
    }
    if let Some(tts) = patch.tts {
        next.tts = tts;
    }
    next.normalize();
    Ok(next)
}

/// One pack in `voiceDownload`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackStatus {
    pub status: &'static str,
    pub message: String,
}

/// Read-only download status for SenseVoice, Cantonese TTS, and English TTS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceDownloadView {
    pub sensevoice: PackStatus,
    pub tts_cantonese: PackStatus,
    pub tts_english: PackStatus,
}

impl VoiceDownloadView {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "sensevoice": {"status": self.sensevoice.status, "message": self.sensevoice.message},
            "ttsCantonese": {
                "status": self.tts_cantonese.status,
                "message": self.tts_cantonese.message,
            },
            "ttsEnglish": {
                "status": self.tts_english.status,
                "message": self.tts_english.message,
            },
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    Downloading,
    Error,
}

#[derive(Clone, Debug)]
struct Slot {
    phase: Phase,
    message: String,
}

impl Default for Slot {
    fn default() -> Self {
        Self {
            phase: Phase::Idle,
            message: String::new(),
        }
    }
}

#[derive(Clone, Debug, Default)]
struct Book {
    sensevoice: Slot,
    tts_cantonese: Slot,
    tts_english: Slot,
}

impl Book {
    fn slot(&self, pack: VoicePack) -> &Slot {
        match pack {
            VoicePack::SenseVoice => &self.sensevoice,
            VoicePack::TtsCantonese => &self.tts_cantonese,
            VoicePack::TtsEnglish => &self.tts_english,
        }
    }

    fn slot_mut(&mut self, pack: VoicePack) -> &mut Slot {
        match pack {
            VoicePack::SenseVoice => &mut self.sensevoice,
            VoicePack::TtsCantonese => &mut self.tts_cantonese,
            VoicePack::TtsEnglish => &mut self.tts_english,
        }
    }
}

struct Inner {
    settings: RwLock<VoiceConfig>,
    book: RwLock<Book>,
    fetcher: RwLock<Arc<dyn VoicePackFetcher>>,
}

/// Live voice config shared by Portal Settings and the Telegram path.
///
/// Cloning is cheap. [`VoiceRuntime::set`] is visible to every clone, the
/// same way SwitchBot keeps its enable flag in process.
#[derive(Clone)]
pub struct VoiceRuntime {
    inner: Arc<Inner>,
}

impl VoiceRuntime {
    pub fn from_config(cfg: VoiceConfig) -> Self {
        Self::with_fetcher(cfg, Arc::new(HttpVoiceFetcher::new()))
    }

    pub fn with_fetcher(cfg: VoiceConfig, fetcher: Arc<dyn VoicePackFetcher>) -> Self {
        let mut cfg = cfg;
        cfg.normalize();
        Self {
            inner: Arc::new(Inner {
                settings: RwLock::new(cfg),
                book: RwLock::new(Book::default()),
                fetcher: RwLock::new(fetcher),
            }),
        }
    }

    pub fn set_fetcher(&self, fetcher: Arc<dyn VoicePackFetcher>) {
        *write(&self.inner.fetcher) = fetcher;
    }

    pub fn snapshot(&self) -> VoiceConfig {
        read(&self.inner.settings).clone()
    }

    pub fn set(&self, mut cfg: VoiceConfig) {
        cfg.normalize();
        *write(&self.inner.settings) = cfg;
    }

    pub fn view(&self) -> VoiceDownloadView {
        let cfg = self.snapshot();
        VoiceDownloadView {
            sensevoice: self.pack_status(VoicePack::SenseVoice, &cfg.model_dir),
            tts_cantonese: self.pack_status(VoicePack::TtsCantonese, &cfg.tts_model_dir),
            tts_english: self.pack_status(VoicePack::TtsEnglish, &cfg.tts_en_model_dir),
        }
    }

    /// Start any missing packs for the live config. Returns immediately.
    /// A failed download is recorded on [`VoiceRuntime::view`] and is not
    /// an error for the caller.
    pub fn kick(&self) {
        let this = self.clone();
        tokio::spawn(async move {
            this.ensure_now().await;
        });
    }

    /// Run the same ensure as [`VoiceRuntime::kick`] and wait for it.
    pub async fn ensure_now(&self) {
        let cfg = self.snapshot();
        if cfg.enabled {
            self.ensure_one(VoicePack::SenseVoice, &cfg.model_dir).await;
        }
        if cfg.enabled && cfg.tts {
            self.ensure_one(VoicePack::TtsCantonese, &cfg.tts_model_dir)
                .await;
            self.ensure_one(VoicePack::TtsEnglish, &cfg.tts_en_model_dir)
                .await;
        }
    }

    fn pack_status(&self, pack: VoicePack, dir: &Path) -> PackStatus {
        if !dir.as_os_str().is_empty() && pack_ready(pack, dir) {
            return PackStatus {
                status: "ready",
                message: "Ready.".to_string(),
            };
        }
        let book = read(&self.inner.book);
        let slot = book.slot(pack);
        match slot.phase {
            Phase::Downloading => PackStatus {
                status: "downloading",
                message: "Downloading.".to_string(),
            },
            Phase::Error => PackStatus {
                status: "error",
                message: slot.message.clone(),
            },
            Phase::Idle => PackStatus {
                status: "absent",
                message: "Not downloaded.".to_string(),
            },
        }
    }

    async fn ensure_one(&self, pack: VoicePack, dir: &Path) {
        if dir.as_os_str().is_empty() {
            self.mark_error(pack, "model directory is not set");
            return;
        }
        if pack_ready(pack, dir) {
            self.mark_ready(pack);
            return;
        }
        if !self.try_begin(pack) {
            return;
        }
        let fetcher = read(&self.inner.fetcher).clone();
        let outcome = fetcher.fetch_pack(pack, dir).await;
        match outcome {
            Ok(()) if pack_ready(pack, dir) => self.mark_ready(pack),
            Ok(()) => self.mark_error(pack, "download finished but model files are still missing"),
            Err(err) => {
                tracing::warn!(
                    pack = pack.id(),
                    error = %err,
                    "voice pack download failed; save is kept"
                );
                self.mark_error(pack, &err);
            }
        }
    }

    fn try_begin(&self, pack: VoicePack) -> bool {
        let mut book = write(&self.inner.book);
        let slot = book.slot_mut(pack);
        if slot.phase == Phase::Downloading {
            return false;
        }
        slot.phase = Phase::Downloading;
        slot.message = "Downloading.".to_string();
        true
    }

    fn mark_ready(&self, pack: VoicePack) {
        let mut book = write(&self.inner.book);
        let slot = book.slot_mut(pack);
        slot.phase = Phase::Idle;
        slot.message.clear();
    }

    fn mark_error(&self, pack: VoicePack, message: &str) {
        let mut book = write(&self.inner.book);
        let slot = book.slot_mut(pack);
        slot.phase = Phase::Error;
        slot.message = short_message(message);
    }
}

fn read<T>(lock: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|err| err.into_inner())
}

fn write<T>(lock: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(|err| err.into_inner())
}

pub fn pack_ready(pack: VoicePack, dir: &Path) -> bool {
    match pack {
        VoicePack::SenseVoice => sensevoice_ready(dir),
        VoicePack::TtsCantonese | VoicePack::TtsEnglish => tts_ready(dir),
    }
}

fn sensevoice_ready(dir: &Path) -> bool {
    dir.join("tokens.txt").is_file()
        && (dir.join("model.int8.onnx").is_file() || dir.join("model.onnx").is_file())
}

fn tts_ready(dir: &Path) -> bool {
    dir.join("model.onnx").is_file() && dir.join("tokens.txt").is_file()
}

fn short_message(message: &str) -> String {
    let one = message.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = one.chars();
    let clipped: String = chars.by_ref().take(160).collect();
    if chars.next().is_some() {
        format!("{clipped}…")
    } else if clipped.is_empty() {
        "download failed".to_string()
    } else {
        clipped
    }
}

struct HttpVoiceFetcher {
    client: reqwest::Client,
}

impl HttpVoiceFetcher {
    fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(DOWNLOAD_TIMEOUT)
            .user_agent(concat!("rustfox-voice-pack/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { client }
    }

    async fn download_file(&self, url: &str, dest: &Path) -> Result<(), String> {
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|err| format!("GET {url}: {err}"))?;
        if !response.status().is_success() {
            return Err(format!("GET {url} returned {}", response.status()));
        }
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|err| err.to_string())?;
        }
        let mut file = tokio::fs::File::create(dest)
            .await
            .map_err(|err| err.to_string())?;
        let mut response = response;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|err| format!("read {url}: {err}"))?
        {
            use tokio::io::AsyncWriteExt;
            file.write_all(&chunk)
                .await
                .map_err(|err| err.to_string())?;
        }
        file.flush().await.map_err(|err| err.to_string())?;
        Ok(())
    }
}

#[async_trait]
impl VoicePackFetcher for HttpVoiceFetcher {
    async fn fetch_pack(&self, pack: VoicePack, dest: &Path) -> Result<(), String> {
        let staging = staging_dir(dest);
        if staging.exists() {
            let _ = tokio::fs::remove_dir_all(&staging).await;
        }
        tokio::fs::create_dir_all(&staging)
            .await
            .map_err(|err| err.to_string())?;
        let unpacked = staging.join("out");
        tokio::fs::create_dir_all(&unpacked)
            .await
            .map_err(|err| err.to_string())?;
        let fetched = match pack {
            VoicePack::SenseVoice => self.fetch_tar_bz2(SENSEVOICE_PACK_URL, &unpacked).await,
            VoicePack::TtsEnglish => self.fetch_tar_bz2(EN_TTS_PACK_URL, &unpacked).await,
            VoicePack::TtsCantonese => self.fetch_yue_files(&unpacked).await,
        };
        if let Err(err) = fetched {
            let _ = tokio::fs::remove_dir_all(&staging).await;
            return Err(format!("{}: {err}", pack.id()));
        }
        if !pack_ready(pack, &unpacked) {
            let _ = tokio::fs::remove_dir_all(&staging).await;
            return Err(format!(
                "{} unpack did not produce the model files",
                pack.id()
            ));
        }
        move_children(&unpacked, dest)?;
        let _ = tokio::fs::remove_dir_all(&staging).await;
        if !pack_ready(pack, dest) {
            return Err(format!(
                "{} is still missing model files after install",
                pack.key()
            ));
        }
        tracing::info!(pack = pack.id(), dir = %dest.display(), "voice pack ready");
        Ok(())
    }
}

impl HttpVoiceFetcher {
    async fn fetch_tar_bz2(&self, url: &str, dest: &Path) -> Result<(), String> {
        let archive = dest.parent().unwrap_or(dest).join(format!(
            "{}.tar.bz2",
            dest.file_name().and_then(|n| n.to_str()).unwrap_or("pack")
        ));
        self.download_file(url, &archive).await?;
        let archive_for_task = archive.clone();
        let dest_for_task = dest.to_path_buf();
        let unpacked =
            tokio::task::spawn_blocking(move || unpack_tar_bz2(&archive_for_task, &dest_for_task))
                .await
                .map_err(|err| format!("unpack task: {err}"))?;
        let _ = tokio::fs::remove_file(&archive).await;
        unpacked
    }

    async fn fetch_yue_files(&self, dest: &Path) -> Result<(), String> {
        for name in YUE_TTS_FILES {
            let url = format!("{YUE_TTS_BASE}/{name}");
            self.download_file(&url, &dest.join(name)).await?;
        }
        alias_model_onnx(dest)
    }
}

fn staging_dir(dest: &Path) -> PathBuf {
    let name = dest
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("voice-pack");
    dest.with_file_name(format!(".{name}.partial"))
}

fn unpack_tar_bz2(archive: &Path, dest: &Path) -> Result<(), String> {
    let file = std::fs::File::open(archive).map_err(|err| format!("open archive: {err}"))?;
    let decoder = bzip2::read::BzDecoder::new(file);
    let mut tar = tar::Archive::new(decoder);
    std::fs::create_dir_all(dest).map_err(|err| err.to_string())?;
    let entries = tar.entries().map_err(|err| format!("tar entries: {err}"))?;
    for entry in entries {
        let mut entry = entry.map_err(|err| format!("tar entry: {err}"))?;
        let raw = entry
            .path()
            .map_err(|err| format!("tar path: {err}"))?
            .to_path_buf();
        let Some(rel) = safe_relative(&raw) else {
            return Err(format!("unsafe tar path {}", raw.display()));
        };
        if rel.as_os_str().is_empty() {
            continue;
        }
        let out = dest.join(&rel);
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            std::fs::create_dir_all(&out).map_err(|err| err.to_string())?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
        }
        if kind.is_symlink() {
            let target = entry
                .link_name()
                .map_err(|err| format!("tar link: {err}"))?
                .unwrap_or_default();
            if target.as_os_str().is_empty() || safe_relative(&target).is_none() {
                return Err(format!("unsafe tar symlink {}", target.display()));
            }
            write_symlink(&target, &out)?;
            continue;
        }
        entry
            .unpack(&out)
            .map_err(|err| format!("unpack {}: {err}", rel.display()))?;
    }
    flatten_single_root(dest)?;
    alias_model_onnx(dest)?;
    Ok(())
}

fn safe_relative(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            Component::RootDir | Component::Prefix(_) | Component::ParentDir => return None,
        }
    }
    Some(out)
}

fn write_symlink(target: &Path, out: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        let _ = std::fs::remove_file(out);
        std::os::unix::fs::symlink(target, out).map_err(|err| format!("symlink: {err}"))?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (target, out);
        Ok(())
    }
}

fn flatten_single_root(dest: &Path) -> Result<(), String> {
    let entries = std::fs::read_dir(dest)
        .map_err(|err| err.to_string())?
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry.file_name() != "pack.tar.bz2"
                && !entry.file_name().to_string_lossy().ends_with(".tar.bz2")
        })
        .collect::<Vec<_>>();
    if entries.len() != 1 {
        return Ok(());
    }
    let root = entries[0].path();
    if !root.is_dir() {
        return Ok(());
    }
    for child in std::fs::read_dir(&root).map_err(|err| err.to_string())? {
        let child = child.map_err(|err| err.to_string())?;
        let target = dest.join(child.file_name());
        replace_path(&target)?;
        std::fs::rename(child.path(), &target)
            .map_err(|err| format!("flatten {}: {err}", child.path().display()))?;
    }
    let _ = std::fs::remove_dir(&root);
    Ok(())
}

fn alias_model_onnx(dest: &Path) -> Result<(), String> {
    if dest.join("model.onnx").is_file() || dest.join("model.int8.onnx").is_file() {
        return Ok(());
    }
    let onnx = std::fs::read_dir(dest)
        .map_err(|err| err.to_string())?
        .filter_map(|entry| entry.ok())
        .find(|entry| {
            entry
                .path()
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("onnx"))
        });
    let Some(onnx) = onnx else {
        return Ok(());
    };
    let name = onnx.file_name();
    write_symlink(Path::new(&name), &dest.join("model.onnx"))
}

fn move_children(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|err| err.to_string())?;
    for entry in std::fs::read_dir(from).map_err(|err| err.to_string())? {
        let entry = entry.map_err(|err| err.to_string())?;
        let target = to.join(entry.file_name());
        replace_path(&target)?;
        std::fs::rename(entry.path(), &target)
            .map_err(|err| format!("install {}: {err}", entry.file_name().to_string_lossy()))?;
    }
    Ok(())
}

fn replace_path(path: &Path) -> Result<(), String> {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return Ok(());
    };
    if meta.file_type().is_dir() {
        std::fs::remove_dir_all(path).map_err(|err| err.to_string())?;
    } else {
        std::fs::remove_file(path).map_err(|err| err.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct ScriptedFetcher {
        fail: bool,
        calls: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl VoicePackFetcher for ScriptedFetcher {
        async fn fetch_pack(&self, pack: VoicePack, dest: &Path) -> Result<(), String> {
            self.calls.lock().unwrap().push(pack.key().to_string());
            if self.fail {
                return Err("disk full".into());
            }
            std::fs::create_dir_all(dest).unwrap();
            match pack {
                VoicePack::SenseVoice => {
                    std::fs::write(dest.join("model.int8.onnx"), b"m").unwrap();
                    std::fs::write(dest.join("tokens.txt"), b"t\n").unwrap();
                }
                VoicePack::TtsCantonese | VoicePack::TtsEnglish => {
                    std::fs::write(dest.join("model.onnx"), b"m").unwrap();
                    std::fs::write(dest.join("tokens.txt"), b"t\n").unwrap();
                }
            }
            Ok(())
        }
    }

    fn dirs(root: &Path) -> VoiceConfig {
        VoiceConfig {
            enabled: true,
            tts: true,
            model_dir: root.join("sensevoice"),
            tts_model_dir: root.join("tts-cantonese"),
            tts_en_model_dir: root.join("tts-english"),
            ..VoiceConfig::default()
        }
    }

    #[test]
    fn english_pack_is_local_piper_lessac_under_tts_en() {
        assert_eq!(EN_TTS_PACK_ID, "vits-piper-en_US-lessac-medium");
        assert!(EN_TTS_PACK_URL.contains(EN_TTS_PACK_ID));
        assert_eq!(EN_TTS_REL_DIR, "models/tts-english");
        assert!(SENSEVOICE_PACK_URL.contains(SENSEVOICE_PACK_ID));
        assert_eq!(YUE_TTS_PACK_ID, "vits-cantonese-hf-xiaomaiiwn");
    }

    #[test]
    fn set_is_visible_on_every_clone() {
        let runtime = VoiceRuntime::with_fetcher(
            VoiceConfig::default(),
            Arc::new(ScriptedFetcher {
                fail: true,
                calls: Mutex::new(Vec::new()),
            }),
        );
        let other = runtime.clone();
        let mut next = runtime.snapshot();
        next.enabled = false;
        next.tts = true;
        next.languages = vec!["en".to_string()];
        runtime.set(next);
        let seen = other.snapshot();
        assert!(!seen.enabled);
        assert!(seen.tts);
        assert_eq!(seen.languages, vec!["en".to_string()]);
        assert_eq!(seen.stt_language_flag(), "en");
    }

    #[test]
    fn merge_rejects_empty_and_unknown_languages() {
        let current = VoiceConfig::default();
        let empty = merge_voice_settings(
            &current,
            &VoiceSettingsPatch {
                languages: Some(vec![]),
                ..VoiceSettingsPatch::default()
            },
        );
        assert!(empty.is_err(), "{empty:?}");
        let unknown = merge_voice_settings(
            &current,
            &VoiceSettingsPatch {
                languages: Some(vec!["fr".into(), "en".into()]),
                ..VoiceSettingsPatch::default()
            },
        );
        assert!(unknown.is_err(), "{unknown:?}");
        assert_eq!(current.languages, vec!["yue".to_string(), "en".to_string()]);
    }

    #[test]
    fn merge_orders_languages_and_keeps_model_dirs() {
        let current = VoiceConfig {
            model_dir: PathBuf::from("/models/sensevoice"),
            ..VoiceConfig::default()
        };
        let next = merge_voice_settings(
            &current,
            &VoiceSettingsPatch {
                enabled: Some(true),
                languages: Some(vec!["ko".into(), "YUE".into(), "en".into(), "yue".into()]),
                tts: Some(true),
            },
        )
        .unwrap();
        assert_eq!(
            next.languages,
            vec!["yue".to_string(), "en".to_string(), "ko".to_string()]
        );
        assert_eq!(next.stt_language_flag(), "auto");
        assert!(next.tts);
        assert_eq!(next.model_dir, PathBuf::from("/models/sensevoice"));
    }

    #[tokio::test]
    async fn failed_download_sets_error_and_does_not_panic() {
        let dir = tempfile::tempdir().unwrap();
        let fetcher = Arc::new(ScriptedFetcher {
            fail: true,
            calls: Mutex::new(Vec::new()),
        });
        let runtime = VoiceRuntime::with_fetcher(dirs(dir.path()), Arc::clone(&fetcher) as Arc<_>);
        runtime.ensure_now().await;
        let view = runtime.view();
        assert_eq!(view.sensevoice.status, "error");
        assert_eq!(view.tts_cantonese.status, "error");
        assert_eq!(view.tts_english.status, "error");
        assert!(view.sensevoice.message.contains("disk full"));
        let calls = fetcher.calls.lock().unwrap().clone();
        assert_eq!(
            calls,
            vec![
                "sensevoice".to_string(),
                "tts-cantonese".to_string(),
                "tts-english".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn enabled_without_tts_downloads_sensevoice_only() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = dirs(dir.path());
        cfg.tts = false;
        let fetcher = Arc::new(ScriptedFetcher {
            fail: false,
            calls: Mutex::new(Vec::new()),
        });
        let runtime = VoiceRuntime::with_fetcher(cfg, Arc::clone(&fetcher) as Arc<_>);
        runtime.ensure_now().await;
        let view = runtime.view();
        assert_eq!(view.sensevoice.status, "ready");
        assert_eq!(view.tts_cantonese.status, "absent");
        assert_eq!(view.tts_english.status, "absent");
        assert_eq!(
            fetcher.calls.lock().unwrap().clone(),
            vec!["sensevoice".to_string()]
        );
    }

    #[tokio::test]
    async fn disabled_master_does_not_download() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = dirs(dir.path());
        cfg.enabled = false;
        cfg.tts = true;
        let fetcher = Arc::new(ScriptedFetcher {
            fail: false,
            calls: Mutex::new(Vec::new()),
        });
        let runtime = VoiceRuntime::with_fetcher(cfg, Arc::clone(&fetcher) as Arc<_>);
        runtime.ensure_now().await;
        assert!(fetcher.calls.lock().unwrap().is_empty());
        assert_eq!(runtime.view().sensevoice.status, "absent");
    }

    #[tokio::test]
    async fn ready_pack_is_not_fetched_again() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dirs(dir.path());
        std::fs::create_dir_all(&cfg.model_dir).unwrap();
        std::fs::write(cfg.model_dir.join("model.int8.onnx"), b"m").unwrap();
        std::fs::write(cfg.model_dir.join("tokens.txt"), b"t\n").unwrap();
        let fetcher = Arc::new(ScriptedFetcher {
            fail: false,
            calls: Mutex::new(Vec::new()),
        });
        let runtime = VoiceRuntime::with_fetcher(cfg, Arc::clone(&fetcher) as Arc<_>);
        runtime.ensure_now().await;
        let calls = fetcher.calls.lock().unwrap().clone();
        assert!(!calls.iter().any(|call| call == "sensevoice"));
        assert!(calls.iter().any(|call| call == "tts-english"));
        assert_eq!(runtime.view().sensevoice.status, "ready");
        assert_eq!(runtime.view().tts_english.status, "ready");
    }
}
