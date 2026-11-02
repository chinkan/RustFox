//! SwitchBot Open API v1.1 connector (ADR-0057).
//!
//! Every request signs `token + t + nonce` with HMAC-SHA256 and the account
//! secret, then sends standard Base64 in the `sign` header. This matches the
//! Python 3 / JavaScript samples and the help-center formula. Config stores
//! `secret:` refs only; the token and secret live in SecretStore.

use base64::Engine as _;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Open API host. Paths are appended, starting with `/v1.1/`.
pub const API_HOST: &str = "https://api.switch-bot.com";

/// Vault name for the Open Token.
pub const TOKEN_SECRET: &str = "switchbot.api_token";
/// Vault name for the secret key used in the HMAC.
pub const SECRET_SECRET: &str = "switchbot.api_secret";

/// Headers SwitchBot v1.1 requires on every request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchBotHeaders {
    pub authorization: String,
    pub sign: String,
    pub t: String,
    pub nonce: String,
    pub content_type: &'static str,
}

/// Base64(HMAC-SHA256(secret, token + t + nonce)).
pub fn sign_v11(token: &str, secret: &str, t_ms: u64, nonce: &str) -> String {
    let payload = format!("{token}{t_ms}{nonce}");
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .expect("HMAC-SHA256 accepts a key of any length");
    mac.update(payload.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

/// Build the v1.1 auth headers for one request.
pub fn auth_headers(token: &str, secret: &str, t_ms: u64, nonce: &str) -> SwitchBotHeaders {
    SwitchBotHeaders {
        authorization: token.to_string(),
        sign: sign_v11(token, secret, t_ms, nonce),
        t: t_ms.to_string(),
        nonce: nonce.to_string(),
        content_type: "application/json",
    }
}

/// One signed Open API call. The transport adds the host.
#[derive(Debug, Clone)]
pub struct SwitchBotRequest {
    pub method: &'static str,
    pub path: String,
    pub headers: SwitchBotHeaders,
    pub body: Option<serde_json::Value>,
}

#[derive(Debug, Clone)]
pub struct SwitchBotResponse {
    pub status: u16,
    pub body: String,
}

#[async_trait::async_trait]
pub trait SwitchBotHttp: Send + Sync {
    async fn exchange(&self, req: &SwitchBotRequest) -> Result<SwitchBotResponse, String>;
}

/// Portal write of Enable / Token / Secret. Token and secret are plaintext
/// here and are sealed into the vault; they are not written to config.
#[derive(Debug, Default)]
pub struct SwitchBotPatch {
    pub enabled: Option<bool>,
    pub token: Option<String>,
    pub secret: Option<String>,
}

#[derive(Debug)]
pub enum SwitchBotPatchError {
    EmptyCredential(&'static str),
    CredentialsRequired,
    Store(String),
}

impl std::fmt::Display for SwitchBotPatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyCredential(which) => write!(f, "empty {which}"),
            Self::CredentialsRequired => write!(f, "credentials required"),
            Self::Store(msg) => write!(f, "{msg}"),
        }
    }
}

/// Masked Portal Settings projection. Secrets are never returned raw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchBotView {
    pub enabled: bool,
    pub token_masked: Option<String>,
    pub secret_masked: Option<String>,
}

/// Builtin `switchbot_*` tools. Registered always; `define` hides them until
/// the connector is enabled and both credentials resolve.
#[derive(Clone)]
pub struct SwitchBotTools {
    inner: std::sync::Arc<Inner>,
}

struct Inner {
    settings: std::sync::RwLock<crate::config::SwitchBotConfig>,
    secrets: std::sync::Arc<dyn crate::secret_store::SecretStore>,
    http: std::sync::Arc<dyn SwitchBotHttp>,
    devices: std::sync::Mutex<Option<(std::time::Instant, String)>>,
}

const DEVICE_LIST_CACHE: std::time::Duration = std::time::Duration::from_secs(30);

impl SwitchBotTools {
    pub fn new(
        settings: crate::config::SwitchBotConfig,
        secrets: std::sync::Arc<dyn crate::secret_store::SecretStore>,
        http: std::sync::Arc<dyn SwitchBotHttp>,
    ) -> Self {
        Self {
            inner: std::sync::Arc::new(Inner {
                settings: std::sync::RwLock::new(settings),
                secrets,
                http,
                devices: std::sync::Mutex::new(None),
            }),
        }
    }

    /// Production client: live Open API host, 20s timeout.
    pub fn from_config(
        settings: crate::config::SwitchBotConfig,
        secrets: std::sync::Arc<dyn crate::secret_store::SecretStore>,
    ) -> Self {
        Self::new(
            settings,
            secrets,
            std::sync::Arc::new(ReqwestSwitchBot::new()),
        )
    }

    /// Placeholder used until `main` shares the process store with the portal.
    pub fn disconnected() -> Self {
        Self::new(
            crate::config::SwitchBotConfig::default(),
            std::sync::Arc::new(crate::secret_store::FakeSecretStore::new()),
            std::sync::Arc::new(RejectHttp),
        )
    }

    pub fn settings(&self) -> crate::config::SwitchBotConfig {
        self.inner
            .settings
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn set_settings(&self, settings: crate::config::SwitchBotConfig) {
        *self
            .inner
            .settings
            .write()
            .unwrap_or_else(|e| e.into_inner()) = settings;
        self.clear_device_cache();
    }

    pub fn view(&self) -> SwitchBotView {
        let cfg = self.settings();
        SwitchBotView {
            enabled: cfg.enabled,
            token_masked: masked_credential(cfg.api_token.as_deref(), self.inner.secrets.as_ref()),
            secret_masked: masked_credential(
                cfg.api_secret.as_deref(),
                self.inner.secrets.as_ref(),
            ),
        }
    }

    fn clear_device_cache(&self) {
        *self.inner.devices.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    fn resolve(&self, cfg: &crate::config::SwitchBotConfig) -> Result<(String, String), String> {
        let token = resolve_one(
            cfg.api_token.as_deref(),
            "token",
            self.inner.secrets.as_ref(),
        )?;
        let secret = resolve_one(
            cfg.api_secret.as_deref(),
            "secret",
            self.inner.secrets.as_ref(),
        )?;
        Ok((token, secret))
    }

    fn ready(&self) -> bool {
        let cfg = self.settings();
        cfg.enabled && self.resolve(&cfg).is_ok()
    }

    async fn call(
        &self,
        method: &'static str,
        path: String,
        body: Option<serde_json::Value>,
        token: &str,
        secret: &str,
    ) -> Result<String, String> {
        let t_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or(0);
        let nonce = uuid::Uuid::new_v4().to_string();
        let req = SwitchBotRequest {
            method,
            path,
            headers: auth_headers(token, secret, t_ms, &nonce),
            body,
        };
        let resp = self.inner.http.exchange(&req).await?;
        interpret(&resp)
    }
}

struct RejectHttp;

#[async_trait::async_trait]
impl SwitchBotHttp for RejectHttp {
    async fn exchange(&self, _req: &SwitchBotRequest) -> Result<SwitchBotResponse, String> {
        Err("SwitchBot HTTP client is not configured.".into())
    }
}

/// reqwest transport. Tests inject [`SwitchBotHttp`] instead of this.
pub struct ReqwestSwitchBot {
    client: reqwest::Client,
    base: String,
}

impl ReqwestSwitchBot {
    pub fn new() -> Self {
        Self::with_base(API_HOST)
    }

    pub fn with_base(base: &str) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            client,
            base: base.trim_end_matches('/').to_string(),
        }
    }
}

impl Default for ReqwestSwitchBot {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl SwitchBotHttp for ReqwestSwitchBot {
    async fn exchange(&self, req: &SwitchBotRequest) -> Result<SwitchBotResponse, String> {
        let method = reqwest::Method::from_bytes(req.method.as_bytes())
            .map_err(|e| format!("SwitchBot method is invalid: {e}"))?;
        let url = format!("{}{}", self.base, req.path);
        let mut builder = self
            .client
            .request(method, &url)
            .header("Authorization", &req.headers.authorization)
            .header("sign", &req.headers.sign)
            .header("t", &req.headers.t)
            .header("nonce", &req.headers.nonce)
            .header("Content-Type", req.headers.content_type);
        if let Some(body) = &req.body {
            builder = builder.json(body);
        }
        let response = builder
            .send()
            .await
            .map_err(|e| format!("SwitchBot request failed: {e}"))?;
        let status = response.status().as_u16();
        let body = response
            .text()
            .await
            .map_err(|e| format!("SwitchBot response could not be read: {e}"))?;
        Ok(SwitchBotResponse { status, body })
    }
}

fn resolve_one(
    raw: Option<&str>,
    which: &str,
    store: &dyn crate::secret_store::SecretStore,
) -> Result<String, String> {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Err(format!(
            "SwitchBot {which} is not configured. Add it in Portal Settings."
        ));
    };
    if let Some(name) = crate::secret_store::parse_secret_ref(raw) {
        match store.get(name) {
            Ok(Some(value)) => {
                let value = value.expose().trim().to_string();
                if value.is_empty() {
                    return Err(format!(
                        "SwitchBot {which} in the vault is empty. Add it in Portal Settings."
                    ));
                }
                Ok(value)
            }
            Ok(None) => Err(format!(
                "SwitchBot {which} is not in the vault. Add it in Portal Settings."
            )),
            Err(e) => Err(format!("SwitchBot could not read the {which}: {e}")),
        }
    } else if let Some(name) = crate::mcp_presets::parse_env_ref(raw) {
        match std::env::var(name) {
            Ok(value) if !value.trim().is_empty() => Ok(value),
            _ => Err(format!(
                "SwitchBot {which} environment variable `{name}` is not set."
            )),
        }
    } else {
        Err(
            "SwitchBot credentials in config must be a secret: or env: reference, not plaintext."
                .into(),
        )
    }
}

fn masked_credential(
    raw: Option<&str>,
    store: &dyn crate::secret_store::SecretStore,
) -> Option<String> {
    let raw = raw?;
    let value = if let Some(name) = crate::secret_store::parse_secret_ref(raw) {
        store
            .get(name)
            .ok()
            .flatten()
            .map(|v| v.expose().to_string())
    } else if let Some(name) = crate::mcp_presets::parse_env_ref(raw) {
        std::env::var(name).ok()
    } else {
        None
    }?;
    Some(mask_secret(value.trim()))
}

fn mask_secret(secret: &str) -> String {
    let chars: Vec<char> = secret.chars().collect();
    if chars.len() <= 8 {
        return "••••".to_string();
    }
    let head: String = chars.iter().take(5).collect();
    let tail: String = chars
        .iter()
        .rev()
        .take(4)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("{head}…{tail}")
}

fn path_id(kind: &str, id: &str) -> Result<String, String> {
    let id = id.trim();
    if id.is_empty()
        || id.contains('/')
        || id.contains('\\')
        || id.contains("..")
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
    {
        return Err(format!(
            "SwitchBot {kind} id cannot be used in the API path. Use an id from switchbot_list_devices or switchbot_list_scenes."
        ));
    }
    Ok(id.to_string())
}

fn interpret(resp: &SwitchBotResponse) -> Result<String, String> {
    if resp.status == 401 {
        return Err(
            "SwitchBot rejected the token or secret (HTTP 401). Check both values in Portal Settings."
                .into(),
        );
    }
    if resp.status == 429 {
        return Err(
            "SwitchBot rate limit reached (HTTP 429). The Open API allows about 10,000 calls per day."
                .into(),
        );
    }
    let parsed: serde_json::Value = match serde_json::from_str(&resp.body) {
        Ok(v) => v,
        Err(_) if (200..300).contains(&resp.status) => return Ok(resp.body.clone()),
        Err(_) => {
            return Err(format!(
                "SwitchBot HTTP {} returned an unexpected body.",
                resp.status
            ));
        }
    };
    if !(200..300).contains(&resp.status) {
        let message = parsed
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("request failed");
        return Err(format!("SwitchBot HTTP {} failed: {message}.", resp.status));
    }
    match parsed.get("statusCode").and_then(|c| c.as_i64()) {
        Some(100) => Ok(success_text(&parsed)),
        Some(code) => {
            let message = parsed.get("message").and_then(|m| m.as_str()).unwrap_or("");
            Err(status_message(code, message))
        }
        None => Ok(resp.body.clone()),
    }
}

fn success_text(parsed: &serde_json::Value) -> String {
    match parsed.get("body") {
        Some(body) if body.as_object().is_some_and(|o| o.is_empty()) || body.is_null() => {
            "SwitchBot accepted the command.".to_string()
        }
        Some(body) => {
            serde_json::to_string(body).unwrap_or_else(|_| "SwitchBot accepted the command.".into())
        }
        None => "SwitchBot accepted the command.".to_string(),
    }
}

fn status_message(code: i64, message: &str) -> String {
    let plain = match code {
        151 => "that device does not support this command",
        152 => "device not found. Call switchbot_list_devices and use a deviceId from the list",
        160 => "that command is not supported",
        161 => "the device is offline",
        171 => "the hub is offline",
        190 => "SwitchBot reported an internal error. Check the command and try again",
        _ => "the API returned an error",
    };
    if message.is_empty() {
        format!("SwitchBot {plain} ({code}).")
    } else {
        format!("SwitchBot {plain} ({code}): {message}.")
    }
}

fn tool_definitions() -> Vec<crate::llm::ToolDefinition> {
    use crate::llm::{FunctionDefinition, ToolDefinition};
    use serde_json::json;
    vec![
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionDefinition {
                name: "switchbot_list_devices".into(),
                description: "List SwitchBot devices and infrared remotes on the account (Open API GET /v1.1/devices). Call this before commanding a device so deviceId is real. Results may be a few seconds stale.".into(),
                parameters: json!({"type":"object","properties":{},"required":[]}),
            },
        },
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionDefinition {
                name: "switchbot_get_status".into(),
                description: "Get one SwitchBot device's current status (power, battery, sensor readings, and other fields the device reports).".into(),
                parameters: json!({
                    "type":"object",
                    "properties":{"deviceId":{"type":"string","description":"deviceId from switchbot_list_devices"}},
                    "required":["deviceId"]
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionDefinition {
                name: "switchbot_command".into(),
                description: "Send a command to a SwitchBot device. commandType is usually \"command\" (use \"customize\" for a learned infrared button). parameter is \"default\" when the command needs none.".into(),
                parameters: json!({
                    "type":"object",
                    "properties":{
                        "deviceId":{"type":"string"},
                        "command":{"type":"string","description":"For example turnOn, turnOff, press, setBrightness"},
                        "parameter":{"description":"String or number. Omit for \"default\"."},
                        "commandType":{"type":"string","enum":["command","customize"]}
                    },
                    "required":["deviceId","command"]
                }),
            },
        },
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionDefinition {
                name: "switchbot_list_scenes".into(),
                description: "List manual SwitchBot scenes (sceneId and sceneName).".into(),
                parameters: json!({"type":"object","properties":{},"required":[]}),
            },
        },
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionDefinition {
                name: "switchbot_execute_scene".into(),
                description: "Run a manual SwitchBot scene by sceneId from switchbot_list_scenes.".into(),
                parameters: json!({
                    "type":"object",
                    "properties":{"sceneId":{"type":"string"}},
                    "required":["sceneId"]
                }),
            },
        },
    ]
}

#[async_trait::async_trait]
impl crate::tool_registry::ToolHandler for SwitchBotTools {
    fn define(&self) -> Vec<crate::llm::ToolDefinition> {
        if self.ready() {
            tool_definitions()
        } else {
            Vec::new()
        }
    }

    async fn execute(
        &self,
        name: &str,
        args: serde_json::Value,
        _ctx: crate::tool_registry::ToolContext,
    ) -> crate::tool_registry::ToolResult {
        if !self.settings().enabled {
            anyhow::bail!("SwitchBot is turned off. Enable it in Portal Settings.");
        }
        let cfg = self.settings();
        let (token, secret) = self.resolve(&cfg).map_err(anyhow::Error::msg)?;
        match name {
            "switchbot_list_devices" => {
                if let Some((at, cached)) = self
                    .inner
                    .devices
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone()
                {
                    if at.elapsed() < DEVICE_LIST_CACHE {
                        return Ok(cached);
                    }
                }
                let text = self
                    .call("GET", "/v1.1/devices".into(), None, &token, &secret)
                    .await
                    .map_err(anyhow::Error::msg)?;
                *self.inner.devices.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some((std::time::Instant::now(), text.clone()));
                Ok(text)
            }
            "switchbot_get_status" => {
                let id = path_id(
                    "device",
                    args.get("deviceId").and_then(|v| v.as_str()).unwrap_or(""),
                )
                .map_err(anyhow::Error::msg)?;
                self.call(
                    "GET",
                    format!("/v1.1/devices/{id}/status"),
                    None,
                    &token,
                    &secret,
                )
                .await
                .map_err(anyhow::Error::msg)
            }
            "switchbot_command" => {
                let id = path_id(
                    "device",
                    args.get("deviceId").and_then(|v| v.as_str()).unwrap_or(""),
                )
                .map_err(anyhow::Error::msg)?;
                let command = args
                    .get("command")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "SwitchBot command needs a command name, for example turnOn."
                        )
                    })?;
                let parameter = args
                    .get("parameter")
                    .cloned()
                    .unwrap_or_else(|| serde_json::Value::String("default".into()));
                let command_type = args
                    .get("commandType")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .unwrap_or("command");
                let body = serde_json::json!({
                    "command": command,
                    "parameter": parameter,
                    "commandType": command_type,
                });
                self.call(
                    "POST",
                    format!("/v1.1/devices/{id}/commands"),
                    Some(body),
                    &token,
                    &secret,
                )
                .await
                .map_err(anyhow::Error::msg)
            }
            "switchbot_list_scenes" => self
                .call("GET", "/v1.1/scenes".into(), None, &token, &secret)
                .await
                .map_err(anyhow::Error::msg),
            "switchbot_execute_scene" => {
                let id = path_id(
                    "scene",
                    args.get("sceneId").and_then(|v| v.as_str()).unwrap_or(""),
                )
                .map_err(anyhow::Error::msg)?;
                self.call(
                    "POST",
                    format!("/v1.1/scenes/{id}/execute"),
                    Some(serde_json::json!({})),
                    &token,
                    &secret,
                )
                .await
                .map_err(anyhow::Error::msg)
            }
            other => anyhow::bail!("Unknown SwitchBot tool: {other}"),
        }
    }
}

/// Seal Portal token/secret into the vault and update the live connector.
/// Config callers persist the returned refs (`secret:switchbot.api_token`,
/// `secret:switchbot.api_secret`) plus `enabled`. Plaintext never goes to disk.
pub fn apply_patch(
    tools: &SwitchBotTools,
    patch: SwitchBotPatch,
) -> Result<crate::config::SwitchBotConfig, SwitchBotPatchError> {
    let mut cfg = tools.settings();
    if let Some(token) = patch.token {
        let token = token.trim();
        if token.is_empty() {
            return Err(SwitchBotPatchError::EmptyCredential("token"));
        }
        cfg.api_token = Some(
            crate::secret_store::store_secret(tools.inner.secrets.as_ref(), TOKEN_SECRET, token)
                .map_err(|e| SwitchBotPatchError::Store(e.to_string()))?,
        );
    }
    if let Some(secret) = patch.secret {
        let secret = secret.trim();
        if secret.is_empty() {
            return Err(SwitchBotPatchError::EmptyCredential("secret"));
        }
        cfg.api_secret = Some(
            crate::secret_store::store_secret(tools.inner.secrets.as_ref(), SECRET_SECRET, secret)
                .map_err(|e| SwitchBotPatchError::Store(e.to_string()))?,
        );
    }
    if let Some(enabled) = patch.enabled {
        if enabled && tools.resolve(&cfg).is_err() {
            return Err(SwitchBotPatchError::CredentialsRequired);
        }
        cfg.enabled = enabled;
    }
    tools.set_settings(cfg.clone());
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Official sample shape: sign = Base64(HMAC-SHA256(secret, token+t+nonce)).
    /// Vector: token=`token`, secret=`secret`, t=`1661927531000`, nonce=`requestID`.
    #[test]
    fn sign_is_base64_hmac_sha256_of_token_t_nonce() {
        let sign = sign_v11("token", "secret", 1_661_927_531_000, "requestID");
        assert_eq!(sign, "A/dW15hTK/WGOGUF0qfl0W9mcem7N+yuMfiENngClUQ=");
    }

    #[test]
    fn auth_headers_carry_token_timestamp_nonce_and_sign() {
        let headers = auth_headers("token", "secret", 1_661_927_531_000, "requestID");
        assert_eq!(headers.authorization, "token");
        assert_eq!(headers.t, "1661927531000");
        assert_eq!(headers.nonce, "requestID");
        assert_eq!(headers.sign, "A/dW15hTK/WGOGUF0qfl0W9mcem7N+yuMfiENngClUQ=");
        assert_eq!(headers.content_type, "application/json");
        assert_ne!(
            headers.sign,
            sign_v11("token", "other-secret", 1_661_927_531_000, "requestID"),
            "a different secret must change the sign"
        );
    }

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use crate::config::SwitchBotConfig;
    use crate::llm::ToolDefinition;
    use crate::secret_store::{FakeSecretStore, SecretStore};
    use crate::tool_registry::{ToolContext, ToolHandler, ToolRegistry, ToolUiMode};
    use serde_json::json;

    const TOKEN: &str = "tok-value";
    const SECRET: &str = "sec-value";

    struct RecHttp {
        status: u16,
        body: Mutex<String>,
        hits: AtomicUsize,
        seen: Mutex<Vec<SwitchBotRequest>>,
    }

    impl RecHttp {
        fn ok(body: &str) -> Arc<Self> {
            Arc::new(Self {
                status: 200,
                body: Mutex::new(body.to_string()),
                hits: AtomicUsize::new(0),
                seen: Mutex::new(Vec::new()),
            })
        }

        fn with_status(status: u16, body: &str) -> Arc<Self> {
            Arc::new(Self {
                status,
                body: Mutex::new(body.to_string()),
                hits: AtomicUsize::new(0),
                seen: Mutex::new(Vec::new()),
            })
        }
    }

    #[async_trait::async_trait]
    impl SwitchBotHttp for RecHttp {
        async fn exchange(&self, req: &SwitchBotRequest) -> Result<SwitchBotResponse, String> {
            self.hits.fetch_add(1, Ordering::SeqCst);
            self.seen.lock().unwrap().push(req.clone());
            Ok(SwitchBotResponse {
                status: self.status,
                body: self.body.lock().unwrap().clone(),
            })
        }
    }

    fn make_tools(
        cfg: SwitchBotConfig,
        store: Arc<FakeSecretStore>,
        http: Arc<RecHttp>,
    ) -> SwitchBotTools {
        SwitchBotTools::new(cfg, store, http)
    }

    fn enabled_refs() -> SwitchBotConfig {
        SwitchBotConfig {
            enabled: true,
            api_token: Some(format!("secret:{TOKEN_SECRET}")),
            api_secret: Some(format!("secret:{SECRET_SECRET}")),
        }
    }

    fn seeded() -> Arc<FakeSecretStore> {
        Arc::new(
            FakeSecretStore::with_secrets([(TOKEN_SECRET, TOKEN), (SECRET_SECRET, SECRET)])
                .unwrap(),
        )
    }

    fn ctx() -> ToolContext {
        ToolContext {
            sandbox_dir: std::path::PathBuf::from("/tmp"),
            home_dir: None,
            sender: Arc::new(TestSender),
            cancel_registry: Arc::new(crate::cancel_registry::CancelRegistry::new()),
            user_id: "u".into(),
            chat_id: "c".into(),
            bot_id: crate::platform::DEFAULT_BOT_ID.to_string(),
            tool_ui_mode: ToolUiMode::Silent,
        }
    }

    fn names(defs: &[ToolDefinition]) -> Vec<String> {
        defs.iter().map(|d| d.function.name.clone()).collect()
    }

    struct TestSender;

    #[async_trait::async_trait]
    impl crate::platform::sender::PlatformSender for TestSender {
        async fn send_message(
            &self,
            _: &str,
            _: &str,
            _: crate::platform::sender::MessageFormat,
        ) -> anyhow::Result<crate::platform::sender::PlatformMessageId> {
            Ok("t:1".into())
        }
        async fn send_file(
            &self,
            _: &str,
            _: &std::path::Path,
            _: Option<&str>,
        ) -> anyhow::Result<crate::platform::sender::PlatformMessageId> {
            Ok("t:1".into())
        }
        async fn show_cancel_button(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> anyhow::Result<crate::platform::sender::PlatformMessageId> {
            Ok("t:1".into())
        }
        async fn edit_message(
            &self,
            _: &str,
            _: &crate::platform::sender::PlatformMessageId,
            _: &str,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn delete_message(
            &self,
            _: &str,
            _: &crate::platform::sender::PlatformMessageId,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn notify_shutdown(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn disabled_or_unconfigured_registers_no_switchbot_tools() {
        let http = RecHttp::ok("{}");
        let off = make_tools(SwitchBotConfig::default(), seeded(), Arc::clone(&http));
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(off));
        assert!(
            names(&reg.all_definitions())
                .iter()
                .all(|n| !n.starts_with("switchbot_")),
            "disabled connector must not offer tools"
        );

        let missing = make_tools(enabled_refs(), Arc::new(FakeSecretStore::new()), http);
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(missing));
        assert!(
            names(&reg.all_definitions()).is_empty()
                || names(&reg.all_definitions())
                    .iter()
                    .all(|n| !n.starts_with("switchbot_")),
            "missing vault secrets must not offer tools"
        );
    }

    #[tokio::test]
    async fn disabled_execute_explains_portal_settings() {
        let http = RecHttp::ok("{}");
        let off = make_tools(SwitchBotConfig::default(), seeded(), Arc::clone(&http));
        let err = off
            .execute("switchbot_list_devices", json!({}), ctx())
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("turned off") && err.contains("Portal Settings"),
            "{err}"
        );
        assert_eq!(http.hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn enabled_without_secrets_explains_what_is_missing() {
        let http = RecHttp::ok("{}");
        let store = Arc::new(FakeSecretStore::new());
        store.set(TOKEN_SECRET, TOKEN).unwrap();
        let tools = make_tools(enabled_refs(), store, Arc::clone(&http));
        let err = tools
            .execute("switchbot_get_status", json!({"deviceId": "ABC"}), ctx())
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("secret") && err.contains("Portal Settings"),
            "{err}"
        );
        assert_eq!(http.hits.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn plaintext_config_is_not_a_credential() {
        let http = RecHttp::ok("{}");
        let cfg = SwitchBotConfig {
            enabled: true,
            api_token: Some("raw-token".into()),
            api_secret: Some(format!("secret:{SECRET_SECRET}")),
        };
        let tools = make_tools(cfg, seeded(), http);
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(tools));
        assert!(names(&reg.all_definitions())
            .iter()
            .all(|n| !n.starts_with("switchbot_")));
    }

    #[tokio::test]
    async fn configured_tools_register_and_list_devices_signs_the_request() {
        let body = r#"{"statusCode":100,"message":"success","body":{"deviceList":[{"deviceId":"ABC123","deviceName":"Lamp"}],"infraredRemoteList":[]}}"#;
        let http = RecHttp::ok(body);
        let tools = make_tools(enabled_refs(), seeded(), Arc::clone(&http));
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(tools.clone()));
        let got = names(&reg.all_definitions());
        for name in [
            "switchbot_list_devices",
            "switchbot_get_status",
            "switchbot_command",
            "switchbot_list_scenes",
            "switchbot_execute_scene",
        ] {
            assert!(got.contains(&name.to_string()), "missing {name} in {got:?}");
        }

        let result = tools
            .execute("switchbot_list_devices", json!({}), ctx())
            .await
            .unwrap();
        assert!(result.contains("Lamp"), "{result}");
        assert!(result.contains("ABC123"), "{result}");
        let seen = http.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].method, "GET");
        assert_eq!(seen[0].path, "/v1.1/devices");
        assert_eq!(seen[0].headers.authorization, TOKEN);
        assert_eq!(seen[0].headers.content_type, "application/json");
        let t: u64 = seen[0].headers.t.parse().unwrap();
        assert_eq!(
            seen[0].headers.sign,
            sign_v11(TOKEN, SECRET, t, &seen[0].headers.nonce)
        );
        assert!(!seen[0].headers.sign.is_empty());
    }

    #[tokio::test]
    async fn command_and_status_use_v11_paths() {
        let http = RecHttp::ok(r#"{"statusCode":100,"message":"success","body":{"power":"on"}}"#);
        let tools = make_tools(enabled_refs(), seeded(), Arc::clone(&http));
        let status = tools
            .execute("switchbot_get_status", json!({"deviceId": "ABC123"}), ctx())
            .await
            .unwrap();
        assert!(status.contains("on"), "{status}");

        let _sent = tools
            .execute(
                "switchbot_command",
                json!({"deviceId": "ABC123", "command": "turnOn"}),
                ctx(),
            )
            .await
            .unwrap();
        let seen = http.seen.lock().unwrap();
        assert_eq!(seen[0].method, "GET");
        assert_eq!(seen[0].path, "/v1.1/devices/ABC123/status");
        assert_eq!(seen[1].method, "POST");
        assert_eq!(seen[1].path, "/v1.1/devices/ABC123/commands");
        let body = seen[1].body.as_ref().unwrap();
        assert_eq!(body["command"], "turnOn");
        assert_eq!(body["parameter"], "default");
        assert_eq!(body["commandType"], "command");
    }

    #[tokio::test]
    async fn command_forwards_numeric_parameter() {
        let http = RecHttp::ok(r#"{"statusCode":100,"message":"success","body":{}}"#);
        let tools = make_tools(enabled_refs(), seeded(), Arc::clone(&http));
        let result = tools
            .execute(
                "switchbot_command",
                json!({
                    "deviceId": "ABC123",
                    "command": "setBrightness",
                    "parameter": 50,
                    "commandType": "command"
                }),
                ctx(),
            )
            .await
            .unwrap();
        assert!(
            result.to_lowercase().contains("accepted") || result.contains("success"),
            "{result}"
        );
        let body = http.seen.lock().unwrap()[0].body.clone().unwrap();
        assert_eq!(body["parameter"], 50);
    }

    #[tokio::test]
    async fn scenes_list_and_execute() {
        let http = RecHttp::ok(
            r#"{"statusCode":100,"message":"success","body":[{"sceneId":"T02-1","sceneName":"Leave"}]}"#,
        );
        let tools = make_tools(enabled_refs(), seeded(), Arc::clone(&http));
        let listed = tools
            .execute("switchbot_list_scenes", json!({}), ctx())
            .await
            .unwrap();
        assert!(listed.contains("Leave"), "{listed}");
        let _ran = tools
            .execute(
                "switchbot_execute_scene",
                json!({"sceneId": "T02-1"}),
                ctx(),
            )
            .await
            .unwrap();
        let seen = http.seen.lock().unwrap();
        assert_eq!(seen[0].path, "/v1.1/scenes");
        assert_eq!(seen[0].method, "GET");
        assert_eq!(seen[1].path, "/v1.1/scenes/T02-1/execute");
        assert_eq!(seen[1].method, "POST");
    }

    #[tokio::test]
    async fn api_errors_are_plain_language_and_hide_secrets() {
        let missing = RecHttp::with_status(
            200,
            r#"{"statusCode":152,"message":"device not found","body":{}}"#,
        );
        let tools = make_tools(enabled_refs(), seeded(), Arc::clone(&missing));
        let err = tools
            .execute("switchbot_get_status", json!({"deviceId": "NOPE"}), ctx())
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("152") && err.to_lowercase().contains("not found"),
            "{err}"
        );
        assert!(!err.contains(TOKEN) && !err.contains(SECRET), "{err}");

        let denied = RecHttp::with_status(401, r#"{"message":"Unauthorized"}"#);
        let tools = make_tools(enabled_refs(), seeded(), Arc::clone(&denied));
        let err = tools
            .execute("switchbot_list_devices", json!({}), ctx())
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("401") && err.contains("Portal Settings"),
            "{err}"
        );

        let limited = RecHttp::with_status(429, "too many");
        let tools = make_tools(enabled_refs(), seeded(), Arc::clone(&limited));
        let err = tools
            .execute("switchbot_list_devices", json!({}), ctx())
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("429") && err.contains("10,000"), "{err}");
    }

    #[tokio::test]
    async fn slash_in_device_id_does_not_call_http() {
        let http = RecHttp::ok("{}");
        let tools = make_tools(enabled_refs(), seeded(), Arc::clone(&http));
        let err = tools
            .execute(
                "switchbot_command",
                json!({"deviceId": "a/b", "command": "turnOn"}),
                ctx(),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(err.to_lowercase().contains("device"), "{err}");
        assert_eq!(http.hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn list_devices_is_cached_briefly() {
        let http = RecHttp::ok(
            r#"{"statusCode":100,"message":"success","body":{"deviceList":[{"deviceName":"Lamp"}]}}"#,
        );
        let tools = make_tools(enabled_refs(), seeded(), Arc::clone(&http));
        let first = tools
            .execute("switchbot_list_devices", json!({}), ctx())
            .await
            .unwrap();
        http.body.lock().unwrap().replace_range(.., r#"{"statusCode":100,"message":"success","body":{"deviceList":[{"deviceName":"Other"}]}}"#);
        let second = tools
            .execute("switchbot_list_devices", json!({}), ctx())
            .await
            .unwrap();
        assert_eq!(first, second);
        assert!(second.contains("Lamp"), "{second}");
        assert_eq!(http.hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn env_ref_overrides_are_advanced_credentials() {
        let name_t = "RUSTFOX_SWITCHBOT_TEST_TOKEN";
        let name_s = "RUSTFOX_SWITCHBOT_TEST_SECRET";
        std::env::set_var(name_t, TOKEN);
        std::env::set_var(name_s, SECRET);
        let cfg = SwitchBotConfig {
            enabled: true,
            api_token: Some(format!("env:{name_t}")),
            api_secret: Some(format!("env:{name_s}")),
        };
        let http =
            RecHttp::ok(r#"{"statusCode":100,"message":"success","body":{"deviceList":[]}}"#);
        let tools = make_tools(cfg, Arc::new(FakeSecretStore::new()), Arc::clone(&http));
        tools
            .execute("switchbot_list_devices", json!({}), ctx())
            .await
            .unwrap();
        assert_eq!(http.seen.lock().unwrap()[0].headers.authorization, TOKEN);
    }

    #[test]
    fn patch_seals_token_and_secret_and_enables() {
        let http = RecHttp::ok("{}");
        let store = Arc::new(FakeSecretStore::new());
        let tools = make_tools(SwitchBotConfig::default(), Arc::clone(&store), http);
        let cfg = apply_patch(
            &tools,
            SwitchBotPatch {
                enabled: Some(true),
                token: Some("  portal-token  ".into()),
                secret: Some("portal-secret".into()),
            },
        )
        .unwrap();
        assert!(cfg.enabled);
        assert_eq!(cfg.api_token.as_deref(), Some("secret:switchbot.api_token"));
        assert_eq!(
            cfg.api_secret.as_deref(),
            Some("secret:switchbot.api_secret")
        );
        assert_eq!(
            store.get(TOKEN_SECRET).unwrap().unwrap().expose(),
            "portal-token"
        );
        assert_eq!(
            store.get(SECRET_SECRET).unwrap().unwrap().expose(),
            "portal-secret"
        );
        assert!(names(&tools.define()).contains(&"switchbot_list_devices".into()));
    }

    #[tokio::test]
    async fn reqwest_client_sends_signed_v11_headers() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1.1/devices"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"statusCode":100,"message":"success","body":{"deviceList":[{"deviceName":"Lamp"}]}}"#,
            ))
            .expect(1)
            .mount(&server)
            .await;
        let http = Arc::new(ReqwestSwitchBot::with_base(&server.uri()));
        let bot = SwitchBotTools::new(enabled_refs(), seeded(), http);
        let result = bot
            .execute("switchbot_list_devices", json!({}), ctx())
            .await
            .unwrap();
        assert!(result.contains("Lamp"), "{result}");
        let reqs = server.received_requests().await.unwrap();
        let headers = &reqs[0].headers;
        assert_eq!(
            headers.get("authorization").unwrap().to_str().unwrap(),
            TOKEN
        );
        let t: u64 = headers.get("t").unwrap().to_str().unwrap().parse().unwrap();
        let nonce = headers.get("nonce").unwrap().to_str().unwrap();
        let sign = headers.get("sign").unwrap().to_str().unwrap();
        assert_eq!(sign, sign_v11(TOKEN, SECRET, t, nonce));
        assert_eq!(
            headers.get("content-type").unwrap().to_str().unwrap(),
            "application/json"
        );
    }

    #[test]
    fn patch_refuses_enable_without_both_secrets() {
        let http = RecHttp::ok("{}");
        let store = Arc::new(FakeSecretStore::new());
        let tools = make_tools(SwitchBotConfig::default(), store, http);
        let err = apply_patch(
            &tools,
            SwitchBotPatch {
                enabled: Some(true),
                token: Some("only-token".into()),
                secret: None,
            },
        )
        .unwrap_err();
        assert!(matches!(err, SwitchBotPatchError::CredentialsRequired));
        assert!(!tools.settings().enabled);
    }
}
