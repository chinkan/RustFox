//! OpenCode detect-only (ADR-0043 Phase 1 / #259).
//!
//! Probes the `opencode` CLI with `--pure` always forced. Surfaces three
//! states for wizard/portal status. Chat and Active selection return a typed
//! error — never a silent hop to the default provider.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

/// Wire `state` values for GET `/api/opencode/status` (snake_case, wizard style).
pub const STATE_ABSENT: &str = "absent";
pub const STATE_NO_CREDS: &str = "no_creds";
pub const STATE_READY: &str = "ready";

/// Typed Phase 1 refusal: chat or Active hit OpenCode while detect-only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenCodeDetectOnlyError {
    /// `chat_completion` / LLM path.
    Chat,
    /// `/models` Active / `set_model` / `default_provider`.
    Active,
}

impl OpenCodeDetectOnlyError {
    pub fn chat() -> Self {
        Self::Chat
    }
    pub fn active() -> Self {
        Self::Active
    }
}

impl std::fmt::Display for OpenCodeDetectOnlyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Chat => write!(
                f,
                "OpenCode is detect-only (ADR-0043 Phase 1); chat is not available"
            ),
            Self::Active => write!(
                f,
                "OpenCode cannot be Active (ADR-0043 Phase 1 detect-only)"
            ),
        }
    }
}

impl std::error::Error for OpenCodeDetectOnlyError {}

/// Status payload for GET `/api/opencode/status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenCodeStatus {
    pub state: &'static str,
    pub model_count: usize,
    pub models: Vec<String>,
    pub message: Option<String>,
}

impl OpenCodeStatus {
    pub fn absent(message: impl Into<String>) -> Self {
        Self {
            state: STATE_ABSENT,
            model_count: 0,
            models: Vec::new(),
            message: Some(message.into()),
        }
    }

    pub fn no_creds(message: Option<String>) -> Self {
        Self {
            state: STATE_NO_CREDS,
            model_count: 0,
            models: Vec::new(),
            message,
        }
    }

    pub fn ready(models: Vec<String>) -> Self {
        let model_count = models.len();
        Self {
            state: STATE_READY,
            model_count,
            models,
            message: None,
        }
    }

    /// Snake_case JSON matching wizard `/api/ollama/local` style.
    pub fn to_json(&self) -> Value {
        json!({
            "state": self.state,
            "model_count": self.model_count,
            "models": self.models,
            "message": self.message,
        })
    }
}

/// Resolve the `opencode` binary: `RUSTFOX_OPENCODE_BIN`, then PATH, then
/// `~/.opencode/bin/opencode` (common install path from #259).
pub fn find_bin() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("RUSTFOX_OPENCODE_BIN") {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return Some(pb);
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join("opencode");
            if candidate.is_file() {
                return Some(candidate);
            }
            #[cfg(windows)]
            {
                let exe = dir.join("opencode.exe");
                if exe.is_file() {
                    return Some(exe);
                }
            }
        }
    }
    if let Some(home) = dirs::home_dir() {
        let candidate = home.join(".opencode").join("bin").join("opencode");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Ensure `--pure` is present in the argv passed to `opencode` (ADR-0043 §5).
pub fn with_pure(args: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(args.len() + 1);
    let mut has_pure = false;
    for a in args {
        if *a == "--pure" {
            has_pure = true;
        }
        out.push((*a).to_string());
    }
    if !has_pure {
        out.push("--pure".to_string());
    }
    out
}

/// Run `opencode` with `--pure` forced. `args` are subcommand tokens before options.
pub fn run_pure(bin: &Path, args: &[&str]) -> std::io::Result<std::process::Output> {
    let argv = with_pure(args);
    Command::new(bin).args(&argv).output()
}

/// Parse `opencode models` stdout: one model id per non-empty line (trim; skip comments).
pub fn parse_models_stdout(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
        .collect()
}

/// True when `opencode auth list` output looks like it has at least one credential.
pub fn auth_list_has_creds(stdout: &str) -> bool {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.contains("no credentials") || lower.contains("not logged") || lower.contains("no auth")
    {
        return false;
    }
    // Any non-empty line that is not a header-only banner counts.
    trimmed.lines().any(|l| {
        let t = l.trim();
        !t.is_empty() && !t.starts_with('#') && !t.eq_ignore_ascii_case("credentials")
    })
}

fn list_models_at(bin: &Path) -> Result<Vec<String>, String> {
    let output = run_pure(bin, &["models"]).map_err(|e| e.to_string())?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "opencode models failed ({}): {}",
            output.status, stderr
        ));
    }
    Ok(parse_models_stdout(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

fn has_creds_at(bin: &Path) -> bool {
    let Ok(output) = run_pure(bin, &["auth", "list"]) else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    auth_list_has_creds(&String::from_utf8_lossy(&output.stdout))
}

/// Probe OpenCode: absent / no_creds / ready(+N). Never panics on a missing binary.
pub fn detect() -> OpenCodeStatus {
    let Some(bin) = find_bin() else {
        return OpenCodeStatus::absent("opencode binary not found");
    };
    match list_models_at(&bin) {
        Ok(models) if !models.is_empty() => OpenCodeStatus::ready(models),
        Ok(models) => {
            if has_creds_at(&bin) {
                OpenCodeStatus::ready(models)
            } else {
                OpenCodeStatus::no_creds(Some("opencode present but no usable credentials".into()))
            }
        }
        Err(e) => {
            if has_creds_at(&bin) {
                // Creds exist but models probe failed — still not "ready" for picker.
                OpenCodeStatus::no_creds(Some(e))
            } else {
                OpenCodeStatus::no_creds(Some(e))
            }
        }
    }
}

/// Blocking detect on a worker thread (wizard/portal handlers).
pub async fn detect_async() -> OpenCodeStatus {
    tokio::task::spawn_blocking(detect)
        .await
        .unwrap_or_else(|_| OpenCodeStatus::absent("opencode detect task failed"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_pure_appends_when_missing() {
        assert_eq!(with_pure(&["models"]), vec!["models", "--pure"]);
        assert_eq!(with_pure(&["auth", "list"]), vec!["auth", "list", "--pure"]);
    }

    #[test]
    fn with_pure_does_not_duplicate() {
        assert_eq!(with_pure(&["models", "--pure"]), vec!["models", "--pure"]);
        assert_eq!(with_pure(&["--pure", "models"]), vec!["--pure", "models"]);
    }

    #[test]
    fn parse_models_stdout_one_per_line() {
        let ids = parse_models_stdout(
            "opencode/glm-5.3-flash-free\nopencode-go/kimi-k3\n\n# skip\n  opencode/gpt-6-luna  \n",
        );
        assert_eq!(
            ids,
            vec![
                "opencode/glm-5.3-flash-free",
                "opencode-go/kimi-k3",
                "opencode/gpt-6-luna"
            ]
        );
    }

    #[test]
    fn auth_list_empty_is_no_creds() {
        assert!(!auth_list_has_creds(""));
        assert!(!auth_list_has_creds("   \n  "));
        assert!(!auth_list_has_creds("No credentials configured\n"));
    }

    #[test]
    fn auth_list_with_rows_has_creds() {
        assert!(auth_list_has_creds("ollama\nOpenCode Go\nxAI\n"));
    }

    #[test]
    fn status_json_shape_absent() {
        let v = OpenCodeStatus::absent("opencode binary not found").to_json();
        assert_eq!(v["state"], STATE_ABSENT);
        assert_eq!(v["model_count"], 0);
        assert!(v["models"].as_array().unwrap().is_empty());
        assert_eq!(v["message"], "opencode binary not found");
    }

    #[test]
    fn status_json_shape_ready() {
        let v = OpenCodeStatus::ready(vec!["opencode/a".into(), "opencode-go/b".into()]).to_json();
        assert_eq!(v["state"], STATE_READY);
        assert_eq!(v["model_count"], 2);
        assert_eq!(v["models"][0], "opencode/a");
        assert!(v["message"].is_null());
    }

    #[test]
    fn detect_only_error_display_mentions_phase1() {
        let chat = OpenCodeDetectOnlyError::chat().to_string();
        assert!(chat.contains("detect-only"));
        assert!(chat.contains("chat"));
        let active = OpenCodeDetectOnlyError::active().to_string();
        assert!(active.contains("Active") || active.contains("active"));
    }

    #[test]
    fn detect_without_binary_is_absent() {
        // Force miss: empty PATH + nonsense RUSTFOX_OPENCODE_BIN.
        let old_bin = std::env::var_os("RUSTFOX_OPENCODE_BIN");
        let old_path = std::env::var_os("PATH");
        std::env::set_var("RUSTFOX_OPENCODE_BIN", "/nonexistent/opencode-rf-259");
        std::env::set_var("PATH", "");
        let status = detect();
        match old_bin {
            Some(v) => std::env::set_var("RUSTFOX_OPENCODE_BIN", v),
            None => std::env::remove_var("RUSTFOX_OPENCODE_BIN"),
        }
        match old_path {
            Some(v) => std::env::set_var("PATH", v),
            None => std::env::remove_var("PATH"),
        }
        assert_eq!(status.state, STATE_ABSENT);
        assert_eq!(status.model_count, 0);
        assert!(status.models.is_empty());
    }
}
