//! Telegram `/config` allowlist editing: validate → `.bak` → atomic write →
//! restore-on-parse-fail. Secrets are never echoed; denylisted keys are rejected.
//!
//! Shared pure logic (no Telegram types) so unit tests cover allow/deny + bak
//! restore without a bot runtime.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::config::Config;

/// Classification for a dotted config key path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyAccess {
    /// Safe to edit via `/config set`. `restart_required` means the process
    /// must exit/reload before the new value is live (v1 = `/restart`).
    Editable { restart_required: bool },
    /// Shown in `/config show` but rejected by `/config set`.
    ReadOnly,
    /// Explicitly forbidden (secrets, shell, wipe, empty allowlist).
    Denied(&'static str),
}

/// One allowlisted editable key.
#[derive(Debug, Clone, Copy)]
pub struct EditableKey {
    /// Canonical dotted path used in `/config set` / docs.
    pub key: &'static str,
    pub description: &'static str,
    pub restart_required: bool,
}

/// Safe subset editable from Telegram (TL lock: allowlist only).
pub const EDITABLE_KEYS: &[EditableKey] = &[
    EditableKey {
        key: "openrouter.model",
        description: "Primary LLM model id (alias: model)",
        restart_required: false, // can also be applied live via Agent::set_model
    },
    EditableKey {
        key: "memory.query_rewriter_enabled",
        description: "Default RAG query rewriting (per-user /queryrewrite still overrides)",
        restart_required: true,
    },
    EditableKey {
        key: "agent.max_iterations",
        description: "Agentic loop iteration cap",
        restart_required: true,
    },
    EditableKey {
        key: "agent.loop_detection.enabled",
        description: "Detect repeated tool-call loops",
        restart_required: true,
    },
    EditableKey {
        key: "learning.skill_extraction_enabled",
        description: "Post-task skill extraction",
        restart_required: true,
    },
    EditableKey {
        key: "general.location",
        description: "Location string injected into the system prompt",
        restart_required: true,
    },
    EditableKey {
        key: "supervisor.default_autonomy_mode",
        description: "Supervisor workflow mode: fast | standard | rigorous",
        restart_required: true,
    },
    EditableKey {
        key: "portal.enabled",
        description: "Embedded web portal master switch",
        restart_required: true,
    },
    EditableKey {
        key: "portal.port",
        description: "Portal HTTP listen port (1–65535)",
        restart_required: true,
    },
    EditableKey {
        key: "portal.bind",
        description: "Portal bind address (loopback/private only)",
        restart_required: true,
    },
    EditableKey {
        key: "portal.user_name",
        description: "Portal chat identity in memory",
        restart_required: true,
    },
];

/// Read-only display keys (TL: sandbox path = display only).
pub const READONLY_KEYS: &[&str] = &["sandbox.allowed_directory"];

/// Result of a successful allowlisted write.
#[derive(Debug, Clone)]
pub struct ApplyResult {
    pub key: String,
    pub restart_required: bool,
    pub bak_path: PathBuf,
}

/// Normalize aliases (`model` → `openrouter.model`) and lowercase.
pub fn normalize_key(raw: &str) -> String {
    let k = raw.trim().to_ascii_lowercase();
    match k.as_str() {
        "model" => "openrouter.model".to_string(),
        "query_rewriter_enabled" | "queryrewrite" => "memory.query_rewriter_enabled".to_string(),
        "max_iterations" => "agent.max_iterations".to_string(),
        "skill_extraction_enabled" => "learning.skill_extraction_enabled".to_string(),
        "location" => "general.location".to_string(),
        "default_autonomy_mode" | "autonomy_mode" => "supervisor.default_autonomy_mode".to_string(),
        other => other.to_string(),
    }
}

/// Classify a key: denylist first, then allowlist / readonly / unknown→denied.
pub fn classify_key(raw: &str) -> KeyAccess {
    let key = normalize_key(raw);

    if let Some(reason) = deny_reason(&key) {
        return KeyAccess::Denied(reason);
    }

    if READONLY_KEYS.contains(&key.as_str()) {
        return KeyAccess::ReadOnly;
    }

    // mcp.<server>.enabled — allowlist pattern (MCP enable toggles).
    if let Some(server) = mcp_enabled_server(&key) {
        if server.is_empty() || !is_safe_mcp_name(server) {
            return KeyAccess::Denied("invalid MCP server name");
        }
        return KeyAccess::Editable {
            restart_required: true,
        };
    }

    if let Some(meta) = EDITABLE_KEYS.iter().find(|k| k.key == key) {
        return KeyAccess::Editable {
            restart_required: meta.restart_required,
        };
    }

    KeyAccess::Denied("key is not on the Telegram edit allowlist")
}

fn mcp_enabled_server(key: &str) -> Option<&str> {
    let rest = key.strip_prefix("mcp.")?;
    let (name, suffix) = rest.rsplit_once('.')?;
    if suffix == "enabled" {
        Some(name)
    } else {
        None
    }
}

fn is_safe_mcp_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn deny_reason(key: &str) -> Option<&'static str> {
    let lower = key.to_ascii_lowercase();

    // Secrets / tokens / API keys (explicit fragments — avoid matching portal.user_name).
    const SECRET_FRAGMENTS: &[&str] = &[
        "bot_token",
        "api_key",
        "auth_token",
        "refresh_token",
        "oauth_client_secret",
        "client_secret",
        "token_sha256",
        "password",
    ];
    for frag in SECRET_FRAGMENTS {
        if lower.contains(frag) {
            return Some("tokens and API keys cannot be edited via Telegram");
        }
    }
    // portal.token / *.token — but not token_expires_at / token_endpoint.
    if lower == "portal.token"
        || (lower.ends_with(".token")
            && !lower.contains("token_expires")
            && !lower.contains("token_endpoint"))
    {
        return Some("tokens and API keys cannot be edited via Telegram");
    }
    if lower.contains("_secret") || lower.ends_with(".secret") {
        return Some("secrets cannot be edited via Telegram");
    }

    // Allowlist wipe / auth change
    if lower.contains("allowed_user_ids") {
        return Some(
            "allowed_user_ids cannot be edited via Telegram (empty allowlist is a hard error)",
        );
    }

    // Arbitrary shell via MCP stdio
    if (lower.contains("mcp_servers") || lower.starts_with("mcp."))
        && (lower.ends_with(".command") || lower.ends_with(".args"))
    {
        return Some("MCP command/args (arbitrary shell) cannot be edited via Telegram");
    }
    if lower.contains(".env.") || lower.ends_with(".env") {
        return Some("MCP env (may contain secrets) cannot be edited via Telegram");
    }

    // Wipe / relocate DB or home
    if lower.contains("database_path") {
        return Some("database_path cannot be edited via Telegram (wipe/relocate risk)");
    }
    if lower == "general.home" {
        return Some("general.home cannot be edited via Telegram");
    }

    None
}

/// Mask a secret for display (never echo raw values).
pub fn mask_secret(secret: &str) -> String {
    let chars: Vec<char> = secret.chars().collect();
    if chars.is_empty() {
        return "(empty)".to_string();
    }
    if chars.len() <= 8 {
        return "••••".to_string();
    }
    let head: String = chars.iter().take(4).collect();
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

/// Human-readable slash map for `/config` help and GUIDE.
pub fn slash_map_markdown() -> String {
    let mut out = String::from(
        "**`/config` slash map**\n\n\
         Usage:\n\
         - `/config` / `/config show` — show editable + read-only values (secrets redacted)\n\
         - `/config keys` — list allowlisted keys\n\
         - `/config set <key> <value>` — validate → `config.toml.bak` → atomic write\n\
         - `/restart` — reply OK, then clean process exit (systemd/launchd/shell brings it back)\n\n\
         **Editable** (allowlist):\n",
    );
    for k in EDITABLE_KEYS {
        let restart = if k.restart_required {
            " — needs `/restart`"
        } else {
            " — live or soft"
        };
        out.push_str(&format!("- `{}` — {}{}\n", k.key, k.description, restart));
    }
    out.push_str(
        "- `mcp.<server>.enabled` — enable/disable a named `[[mcp_servers]]` entry — needs `/restart`\n",
    );
    out.push_str("\n**Read-only display:**\n");
    for k in READONLY_KEYS {
        out.push_str(&format!("- `{k}`\n"));
    }
    out.push_str(
        "\n**Denied:** `bot_token` / API keys / provider secrets; MCP `command`/`args`/`env`; \
         `allowed_user_ids` (incl. emptying); `database_path`; `general.home`; portal tokens.\n",
    );
    out
}

/// Build a redacted summary from the live [`Config`] (no raw secrets).
pub fn format_show(cfg: &Config) -> String {
    let mut lines = Vec::new();
    lines.push("**Config (redacted)**".to_string());
    lines.push(String::new());
    lines.push("_Editable:_".to_string());
    lines.push(format!("- `openrouter.model` = `{}`", cfg.openrouter.model));
    lines.push(format!(
        "- `memory.query_rewriter_enabled` = `{}`",
        cfg.memory.query_rewriter_enabled
    ));
    lines.push(format!(
        "- `agent.max_iterations` = `{}`",
        cfg.agent.max_iterations
    ));
    lines.push(format!(
        "- `agent.loop_detection.enabled` = `{}`",
        cfg.agent.loop_detection.enabled
    ));
    lines.push(format!(
        "- `learning.skill_extraction_enabled` = `{}`",
        cfg.learning.skill_extraction_enabled
    ));
    let loc = cfg
        .general
        .as_ref()
        .and_then(|g| g.location.as_deref())
        .unwrap_or("(unset)");
    lines.push(format!("- `general.location` = `{loc}`"));
    lines.push(format!(
        "- `supervisor.default_autonomy_mode` = `{}`",
        cfg.supervisor.default_autonomy_mode
    ));
    lines.push(format!("- `portal.enabled` = `{}`", cfg.portal.enabled));
    lines.push(format!("- `portal.port` = `{}`", cfg.portal.port));
    lines.push(format!("- `portal.bind` = `{}`", cfg.portal.bind));
    lines.push(format!("- `portal.user_name` = `{}`", cfg.portal.user_name));

    if !cfg.mcp_servers.is_empty() {
        lines.push(String::new());
        lines.push("_MCP servers (enable toggles need `/restart`):_".to_string());
        for s in &cfg.mcp_servers {
            lines.push(format!("- `mcp.{}.enabled` = `{}`", s.name, s.enabled));
        }
    }

    lines.push(String::new());
    lines.push("_Read-only:_".to_string());
    lines.push(format!(
        "- `sandbox.allowed_directory` = `{}`",
        cfg.sandbox.allowed_directory.display()
    ));

    lines.push(String::new());
    lines.push("_Secrets (redacted — not editable via chat):_".to_string());
    lines.push(format!(
        "- `telegram.bot_token` = `{}`",
        mask_secret(&cfg.telegram.bot_token)
    ));
    lines.push(format!(
        "- `openrouter.api_key` = `{}`",
        mask_secret(&cfg.openrouter.api_key)
    ));
    if let Some(emb) = &cfg.embedding {
        lines.push(format!(
            "- `embedding.api_key` = `{}`",
            mask_secret(&emb.api_key)
        ));
    }
    if cfg.portal.token.is_some() || cfg.portal.token_sha256.is_some() {
        lines.push("- `portal.token` / `token_sha256` = `••••`".to_string());
    }

    lines.push(String::new());
    lines.push("Use `/config set <key> <value>` then `/restart` when needed.".to_string());
    lines.join("\n")
}

/// Validate → bak → atomic write. On post-write parse failure, restore `.bak` and error.
pub fn apply_config_edit(path: &Path, raw_key: &str, raw_value: &str) -> Result<ApplyResult> {
    let key = normalize_key(raw_key);
    match classify_key(&key) {
        KeyAccess::Editable { restart_required } => {
            apply_editable(path, &key, raw_value, restart_required)
        }
        KeyAccess::ReadOnly => bail!("`{key}` is read-only (display only)"),
        KeyAccess::Denied(reason) => bail!("denied `{key}`: {reason}"),
    }
}

fn apply_editable(
    path: &Path,
    key: &str,
    raw_value: &str,
    restart_required: bool,
) -> Result<ApplyResult> {
    let original = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;

    let mut doc: toml::Value =
        toml::from_str(&original).context("Failed to parse config.toml before edit")?;

    apply_key_to_doc(&mut doc, key, raw_value)?;

    let new_content =
        toml::to_string_pretty(&doc).context("Failed to serialize config after edit")?;

    let bak = write_config_validated(path, &new_content)?;

    Ok(ApplyResult {
        key: key.to_string(),
        restart_required,
        bak_path: bak,
    })
}

/// Shared config.toml write path used by `/config set`, `Agent::set_model`, and
/// other callers: validate → `.bak` → atomic write → restore-on-post-write-fail.
///
/// Returns the backup path (`config.toml.bak`). The bak file is only created
/// when `path` already existed before the write.
pub fn write_config_validated(path: &Path, new_content: &str) -> Result<PathBuf> {
    // Pre-write validate (parse + bots normalize). Abort before touching bak/disk.
    validate_config_str(new_content).context("Validation failed; config.toml not modified")?;

    let bak = backup_path(path);
    if path.exists() {
        std::fs::copy(path, &bak)
            .with_context(|| format!("Failed to write backup {}", bak.display()))?;
        // The bak can hold pre-seal plaintext secrets: owner-only.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bak, std::fs::Permissions::from_mode(0o600))
                .with_context(|| format!("Failed to chmod 600 {}", bak.display()))?;
        }
    }

    atomic_write(path, new_content)
        .with_context(|| format!("Failed to write {}", path.display()))?;

    // Post-write validate; restore bak on failure.
    if let Err(e) = validate_config_str(&std::fs::read_to_string(path).unwrap_or_default()) {
        let _ = std::fs::copy(&bak, path);
        bail!(
            "Post-write parse failed ({e}); restored from {}. Aborting.",
            bak.display()
        );
    }

    Ok(bak)
}

/// Persist a live model change to `config.toml` via [`write_config_validated`].
///
/// Updates the matching `[[provider]]` entry's `model`, or falls back to the
/// legacy `[openrouter]` section when `provider_name == "openrouter"`.
/// Returns the `.bak` path.
pub fn persist_model_edit(path: &Path, provider_name: &str, actual_model: &str) -> Result<PathBuf> {
    if actual_model.is_empty() {
        bail!("Model ID cannot be empty");
    }

    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let mut doc: toml::Value =
        toml::from_str(&content).context("Failed to parse config.toml before model edit")?;

    let mut found_in_array = false;
    if let Some(provider_array) = doc.get_mut("provider").and_then(|v| v.as_array_mut()) {
        for entry in provider_array.iter_mut() {
            if let Some(table) = entry.as_table_mut() {
                if table.get("name").and_then(|v| v.as_str()) == Some(provider_name) {
                    table.insert(
                        "model".to_string(),
                        toml::Value::String(actual_model.to_string()),
                    );
                    found_in_array = true;
                }
            }
        }
    }

    if !found_in_array && provider_name == "openrouter" && doc.get("openrouter").is_some() {
        if let Some(table) = doc.get_mut("openrouter").and_then(|v| v.as_table_mut()) {
            table.insert(
                "model".to_string(),
                toml::Value::String(actual_model.to_string()),
            );
        }
    }

    let new_content =
        toml::to_string_pretty(&doc).context("Failed to serialize config after model edit")?;
    write_config_validated(path, &new_content)
}

fn backup_path(path: &Path) -> PathBuf {
    // config.toml → config.toml.bak (same as portal/settings.rs)
    path.with_extension("toml.bak")
}

fn atomic_write(path: &Path, content: &str) -> Result<()> {
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, content)
        .with_context(|| format!("Failed to write temp {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("Failed to rename {} → {}", tmp.display(), path.display()))?;
    Ok(())
}

/// Parse + `normalize_bots` without resolving home paths (enough for edit safety).
pub fn validate_config_str(content: &str) -> Result<()> {
    let mut config: Config =
        toml::from_str(content).context("TOML parse / schema validation failed")?;
    config
        .normalize_bots()
        .context("bots / telegram validation failed")?;
    Ok(())
}

fn apply_key_to_doc(doc: &mut toml::Value, key: &str, raw_value: &str) -> Result<()> {
    if let Some(server) = mcp_enabled_server(key) {
        let enabled = parse_bool(raw_value)?;
        set_mcp_enabled(doc, server, enabled)?;
        return Ok(());
    }

    match key {
        "openrouter.model" => {
            let v = raw_value.trim();
            if v.is_empty() {
                bail!("model must be non-empty");
            }
            ensure_table(doc, "openrouter")
                .insert("model".into(), toml::Value::String(v.to_string()));
        }
        "memory.query_rewriter_enabled" => {
            let v = parse_bool(raw_value)?;
            ensure_table(doc, "memory")
                .insert("query_rewriter_enabled".into(), toml::Value::Boolean(v));
        }
        "agent.max_iterations" => {
            let n: u32 = raw_value
                .trim()
                .parse()
                .context("agent.max_iterations must be a positive integer")?;
            if n == 0 {
                bail!("agent.max_iterations must be >= 1");
            }
            ensure_table(doc, "agent")
                .insert("max_iterations".into(), toml::Value::Integer(n as i64));
        }
        "agent.loop_detection.enabled" => {
            let v = parse_bool(raw_value)?;
            let agent = ensure_table(doc, "agent");
            let ld = agent
                .entry("loop_detection")
                .or_insert_with(|| toml::Value::Table(Default::default()));
            let ld_table = ld
                .as_table_mut()
                .context("agent.loop_detection must be a table")?;
            ld_table.insert("enabled".into(), toml::Value::Boolean(v));
        }
        "learning.skill_extraction_enabled" => {
            let v = parse_bool(raw_value)?;
            ensure_table(doc, "learning")
                .insert("skill_extraction_enabled".into(), toml::Value::Boolean(v));
        }
        "general.location" => {
            ensure_table(doc, "general").insert(
                "location".into(),
                toml::Value::String(raw_value.trim().to_string()),
            );
        }
        "supervisor.default_autonomy_mode" => {
            let mode = raw_value.trim();
            if ![
                "fast",
                "standard",
                "rigorous",
                "default",
                "autopilot",
                "plan",
            ]
            .contains(&mode)
            {
                bail!(
                    "supervisor.default_autonomy_mode must be one of: \
                     fast, standard, rigorous (also accepted: default, autopilot, plan)"
                );
            }
            ensure_table(doc, "supervisor").insert(
                "default_autonomy_mode".into(),
                toml::Value::String(mode.to_string()),
            );
        }
        "portal.enabled" => {
            let v = parse_bool(raw_value)?;
            ensure_table(doc, "portal").insert("enabled".into(), toml::Value::Boolean(v));
        }
        "portal.port" => {
            let port: u16 = raw_value
                .trim()
                .parse()
                .context("portal.port must be an integer 1–65535")?;
            if port == 0 {
                bail!("portal.port must be 1–65535");
            }
            ensure_table(doc, "portal").insert("port".into(), toml::Value::Integer(port as i64));
        }
        "portal.bind" => {
            let bind = raw_value.trim();
            if bind.is_empty() {
                bail!("portal.bind must be non-empty");
            }
            ensure_table(doc, "portal")
                .insert("bind".into(), toml::Value::String(bind.to_string()));
        }
        "portal.user_name" => {
            let name = raw_value.trim();
            if name.is_empty() {
                bail!("portal.user_name must be non-empty");
            }
            ensure_table(doc, "portal")
                .insert("user_name".into(), toml::Value::String(name.to_string()));
        }
        other => bail!("internal: unhandled allowlisted key `{other}`"),
    }
    Ok(())
}

fn set_mcp_enabled(doc: &mut toml::Value, server: &str, enabled: bool) -> Result<()> {
    let servers = doc
        .get_mut("mcp_servers")
        .and_then(|v| v.as_array_mut())
        .with_context(|| format!("no [[mcp_servers]] entry named `{server}`"))?;

    for entry in servers.iter_mut() {
        let Some(table) = entry.as_table_mut() else {
            continue;
        };
        if table.get("name").and_then(|v| v.as_str()) == Some(server) {
            table.insert("enabled".into(), toml::Value::Boolean(enabled));
            return Ok(());
        }
    }
    bail!("no [[mcp_servers]] entry named `{server}`");
}

fn ensure_table<'a>(doc: &'a mut toml::Value, section: &str) -> &'a mut toml::value::Table {
    if doc.get(section).is_none() {
        if let Some(t) = doc.as_table_mut() {
            t.insert(section.to_string(), toml::Value::Table(Default::default()));
        }
    }
    doc.get_mut(section)
        .and_then(|v| v.as_table_mut())
        .expect("section is a table")
}

fn parse_bool(raw: &str) -> Result<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        other => bail!("expected boolean (true/false), got `{other}`"),
    }
}

/// Result of trying to replace the fresh-install allowlist sentinel `[0]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnownedClaim {
    /// Not the unowned sentinel, or this sender is already on the real list.
    /// Nothing was written.
    Unchanged,
    /// This sender replaced `[0]` and the file was persisted.
    Claimed,
    /// The bot already has a real allowlist and this sender is not on it.
    /// Nothing was written.
    Rejected,
}

fn claim_mutex() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    &LOCK
}

fn section_allowlist(section: &toml::Value) -> Option<Vec<u64>> {
    let arr = section.get("allowed_user_ids")?.as_array()?;
    let mut ids = Vec::with_capacity(arr.len());
    for item in arr {
        let n = item.as_integer()?;
        if n < 0 {
            return None;
        }
        ids.push(n as u64);
    }
    Some(ids)
}

fn write_section_allowlist(section: &mut toml::Value, sender: u64) -> bool {
    let Some(table) = section.as_table_mut() else {
        return false;
    };
    table.insert(
        "allowed_user_ids".into(),
        toml::Value::Array(vec![toml::Value::Integer(sender as i64)]),
    );
    true
}

/// First inbound Telegram user replaces `allowed_user_ids = [0]` for one bot.
///
/// `0` is only the unowned sentinel. A real list is never overwritten. An
/// empty list is never written. When `[[bots]]` is non-empty, only the bot
/// whose `id` equals `bot_id` is touched. A `[telegram]`-only file is claimed
/// only for the synthesized id `default`.
///
/// Re-reads the file under a process lock so two messages cannot both claim.
pub fn claim_unowned_bot(path: &Path, bot_id: &str, sender: u64) -> Result<UnownedClaim> {
    if sender == 0 || !path.exists() {
        return Ok(UnownedClaim::Unchanged);
    }
    let _guard = claim_mutex()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let original = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let mut doc: toml::Value =
        toml::from_str(&original).context("Failed to parse config.toml before allowlist claim")?;
    let Some(root) = doc.as_table_mut() else {
        return Ok(UnownedClaim::Unchanged);
    };

    let bots_present = root
        .get("bots")
        .and_then(|v| v.as_array())
        .is_some_and(|bots| !bots.is_empty());

    let ids: Option<()> = if bots_present {
        let Some(bots) = root.get_mut("bots").and_then(|v| v.as_array_mut()) else {
            return Ok(UnownedClaim::Unchanged);
        };
        let Some(bot) = bots.iter_mut().find(|bot| {
            bot.get("id")
                .and_then(|v| v.as_str())
                .is_some_and(|id| id.trim() == bot_id.trim())
        }) else {
            return Ok(UnownedClaim::Unchanged);
        };
        match section_allowlist(bot) {
            Some(ids) if ids == [0] => {
                if !write_section_allowlist(bot, sender) {
                    return Ok(UnownedClaim::Unchanged);
                }
                None
            }
            Some(ids) if ids.contains(&sender) => return Ok(UnownedClaim::Unchanged),
            Some(_) => return Ok(UnownedClaim::Rejected),
            None => return Ok(UnownedClaim::Unchanged),
        }
    } else if bot_id.trim() == "default" {
        let Some(telegram) = root.get_mut("telegram") else {
            return Ok(UnownedClaim::Unchanged);
        };
        match section_allowlist(telegram) {
            Some(ids) if ids == [0] => {
                if !write_section_allowlist(telegram, sender) {
                    return Ok(UnownedClaim::Unchanged);
                }
                None
            }
            Some(ids) if ids.contains(&sender) => return Ok(UnownedClaim::Unchanged),
            Some(_) => return Ok(UnownedClaim::Rejected),
            None => return Ok(UnownedClaim::Unchanged),
        }
    } else {
        return Ok(UnownedClaim::Unchanged);
    };
    let _ = ids;

    let new_content =
        toml::to_string_pretty(&doc).context("Failed to serialize config after allowlist claim")?;
    write_config_validated(path, &new_content)?;
    Ok(UnownedClaim::Claimed)
}

/// Restore `path` from its `.bak` sibling (used by tests + failure path).
pub fn restore_from_bak(path: &Path) -> Result<()> {
    let bak = backup_path(path);
    if !bak.exists() {
        bail!("backup {} not found", bak.display());
    }
    std::fs::copy(&bak, path).with_context(|| {
        format!(
            "Failed to restore {} from {}",
            path.display(),
            bak.display()
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    fn minimal_toml() -> String {
        r#"
[telegram]
bot_token = "123456:ABC-SECRETTOKEN"
allowed_user_ids = [42]

[openrouter]
api_key = "sk-or-v1-SECRETKEY000"
model = "moonshotai/kimi-k2.6"

[sandbox]
allowed_directory = "/tmp/sandbox"

[[mcp_servers]]
name = "git"
command = "uvx"
args = ["mcp-server-git"]
"#
        .to_string()
    }

    fn write_cfg(dir: &tempfile::TempDir, content: &str) -> PathBuf {
        let path = dir.path().join("config.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        path
    }

    #[test]
    fn allowlist_accepts_known_safe_keys() {
        for key in [
            "openrouter.model",
            "model",
            "portal.enabled",
            "portal.port",
            "memory.query_rewriter_enabled",
            "mcp.git.enabled",
        ] {
            assert!(
                matches!(classify_key(key), KeyAccess::Editable { .. }),
                "expected editable: {key}"
            );
        }
    }

    #[test]
    fn denylist_rejects_secrets_and_dangerous_keys() {
        for (key, _) in [
            ("telegram.bot_token", "token"),
            ("openrouter.api_key", "api"),
            ("embedding.api_key", "api"),
            ("portal.token", "token"),
            ("portal.token_sha256", "token"),
            ("provider.0.api_key", "api"),
            ("mcp_servers.0.auth_token", "token"),
            ("mcp_servers.0.refresh_token", "token"),
            ("mcp_servers.0.oauth_client_secret", "secret"),
            ("mcp_servers.0.command", "shell"),
            ("mcp.git.command", "shell"),
            ("telegram.allowed_user_ids", "allowlist"),
            ("bots.0.allowed_user_ids", "allowlist"),
            ("memory.database_path", "db"),
            ("general.home", "home"),
        ] {
            assert!(
                matches!(classify_key(key), KeyAccess::Denied(_)),
                "expected denied: {key}"
            );
        }
    }

    #[test]
    fn sandbox_path_is_read_only() {
        assert_eq!(
            classify_key("sandbox.allowed_directory"),
            KeyAccess::ReadOnly
        );
        let dir = tempdir().unwrap();
        let path = write_cfg(&dir, &minimal_toml());
        let err = apply_config_edit(&path, "sandbox.allowed_directory", "/evil")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("read-only"),
            "sandbox write must fail read-only: {err}"
        );
    }

    #[test]
    fn unknown_key_is_denied() {
        assert!(matches!(
            classify_key("agent.not_a_real_knob"),
            KeyAccess::Denied(_)
        ));
    }

    #[test]
    fn apply_writes_bak_and_updates_value() {
        let dir = tempdir().unwrap();
        let path = write_cfg(&dir, &minimal_toml());
        let result = apply_config_edit(&path, "portal.port", "9090").unwrap();
        assert!(result.restart_required);
        assert!(result.bak_path.exists(), "bak must exist");
        let bak_content = std::fs::read_to_string(&result.bak_path).unwrap();
        assert!(
            bak_content.contains("kimi-k2.6"),
            "bak should be pre-edit snapshot"
        );
        let new_content = std::fs::read_to_string(&path).unwrap();
        assert!(
            new_content.contains("9090") || new_content.contains("port = 9090"),
            "new config should contain port 9090: {new_content}"
        );
        // Secrets must still be present in file (we don't strip them) but show must redact
        let cfg: Config = {
            let mut c: Config = toml::from_str(&new_content).unwrap();
            c.normalize_bots().unwrap();
            c
        };
        let show = format_show(&cfg);
        assert!(
            !show.contains("SECRETTOKEN"),
            "show must not leak bot token"
        );
        assert!(!show.contains("SECRETKEY"), "show must not leak api key");
        assert!(show.contains("••••") || show.contains("…"));
    }

    #[test]
    fn apply_rejects_deny_without_touching_file() {
        let dir = tempdir().unwrap();
        let path = write_cfg(&dir, &minimal_toml());
        let before = std::fs::read_to_string(&path).unwrap();
        let err = apply_config_edit(&path, "openrouter.api_key", "sk-leak")
            .unwrap_err()
            .to_string();
        assert!(err.contains("denied") || err.contains("API"), "{err}");
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(before, after, "denylist reject must not modify config");
        assert!(!path.with_extension("toml.bak").exists());
    }

    #[test]
    fn bak_restore_recovers_previous_content() {
        let dir = tempdir().unwrap();
        let path = write_cfg(&dir, &minimal_toml());
        let original = std::fs::read_to_string(&path).unwrap();
        apply_config_edit(&path, "portal.enabled", "true").unwrap();
        assert_ne!(std::fs::read_to_string(&path).unwrap(), original);
        restore_from_bak(&path).unwrap();
        let restored = std::fs::read_to_string(&path).unwrap();
        // Pretty-print may differ; compare parsed model equality via token presence
        assert!(
            restored.contains("123456:ABC-SECRETTOKEN"),
            "restore must bring back original secrets"
        );
        // Re-apply and simulate post-write failure restore path manually
        apply_config_edit(&path, "general.location", "Hong Kong").unwrap();
        // Corrupt the file as if write went bad
        std::fs::write(&path, "not = valid = toml [[[").unwrap();
        restore_from_bak(&path).unwrap();
        validate_config_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    }

    #[test]
    fn post_write_parse_fail_restores_bak() {
        // Force a path where pre-validate passes but we simulate corruption by
        // testing restore_from_bak after a deliberate bad write alongside bak.
        let dir = tempdir().unwrap();
        let path = write_cfg(&dir, &minimal_toml());
        let _ = apply_config_edit(&path, "portal.bind", "127.0.0.1").unwrap();
        let bak = path.with_extension("toml.bak");
        assert!(bak.exists());
        // Corrupt live file
        std::fs::write(&path, "[[[broken").unwrap();
        assert!(validate_config_str(&std::fs::read_to_string(&path).unwrap()).is_err());
        restore_from_bak(&path).unwrap();
        validate_config_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    }

    #[test]
    fn mcp_enabled_toggle_updates_named_server() {
        let dir = tempdir().unwrap();
        let path = write_cfg(&dir, &minimal_toml());
        apply_config_edit(&path, "mcp.git.enabled", "false").unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(
            content.contains("enabled = false") || content.contains("enabled=false"),
            "expected enabled=false in config: {content}"
        );
        let err = apply_config_edit(&path, "mcp.missing.enabled", "true")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("no [[mcp_servers]]") || err.contains("missing"),
            "{err}"
        );
    }

    #[test]
    fn empty_allowed_user_ids_still_fails_validation() {
        let bad = r#"
[telegram]
bot_token = "t"
allowed_user_ids = []
[openrouter]
api_key = "k"
model = "m"
"#;
        let err = format!("{:#}", validate_config_str(bad).unwrap_err());
        assert!(
            err.contains("allowed_user_ids"),
            "empty allowlist must fail validate: {err}"
        );
    }

    #[test]
    fn mask_secret_never_returns_raw() {
        let raw = "sk-or-v1-abcdefghijklmnopqrstuvwxyz";
        let masked = mask_secret(raw);
        assert!(!masked.contains("abcdefghijklmnopqrst"));
        assert_eq!(mask_secret("short"), "••••");
    }

    #[test]
    fn persist_model_edit_writes_bak_and_updates_model() {
        let dir = tempdir().unwrap();
        let path = write_cfg(&dir, &minimal_toml());
        let original = std::fs::read_to_string(&path).unwrap();
        assert!(original.contains("moonshotai/kimi-k2.6"));

        let bak = persist_model_edit(&path, "openrouter", "anthropic/claude-sonnet-4").unwrap();
        assert!(
            bak.exists(),
            "set_model-equivalent persist must create .bak"
        );
        let bak_content = std::fs::read_to_string(&bak).unwrap();
        assert!(
            bak_content.contains("moonshotai/kimi-k2.6"),
            "bak must be pre-edit snapshot"
        );

        let new_content = std::fs::read_to_string(&path).unwrap();
        assert!(
            new_content.contains("anthropic/claude-sonnet-4"),
            "persisted model missing: {new_content}"
        );
        assert!(
            !new_content.contains("moonshotai/kimi-k2.6"),
            "old model should be replaced: {new_content}"
        );
        validate_config_str(&new_content).unwrap();
    }

    #[test]
    fn persist_model_edit_restore_on_post_write_fail() {
        // Bak is always the *pre-edit* snapshot. After two persists, bak holds
        // the first persist's content; restore recovers that.
        let dir = tempdir().unwrap();
        let path = write_cfg(&dir, &minimal_toml());
        persist_model_edit(&path, "openrouter", "openai/gpt-4o").unwrap();
        let bak = persist_model_edit(&path, "openrouter", "anthropic/claude-sonnet-4").unwrap();
        assert!(bak.exists());
        let bak_content = std::fs::read_to_string(&bak).unwrap();
        assert!(
            bak_content.contains("openai/gpt-4o"),
            "second persist bak must snapshot first persist: {bak_content}"
        );

        // Invalid content must not touch disk (pre-validate abort).
        let before = std::fs::read_to_string(&path).unwrap();
        assert!(before.contains("anthropic/claude-sonnet-4"));
        let err = write_config_validated(&path, "not = valid = toml [[[")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("Validation failed") || err.contains("parse"),
            "{err}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

        // Corrupt live file then restore from bak (pre-second-edit = gpt-4o).
        std::fs::write(&path, "[[[broken").unwrap();
        restore_from_bak(&path).unwrap();
        let restored = std::fs::read_to_string(&path).unwrap();
        assert!(
            restored.contains("openai/gpt-4o"),
            "restore must recover bak (first persist): {restored}"
        );
        validate_config_str(&restored).unwrap();
    }

    #[test]
    fn persist_model_edit_rejects_empty_model() {
        let dir = tempdir().unwrap();
        let path = write_cfg(&dir, &minimal_toml());
        let before = std::fs::read_to_string(&path).unwrap();
        let err = persist_model_edit(&path, "openrouter", "")
            .unwrap_err()
            .to_string();
        assert!(err.contains("empty"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        assert!(!path.with_extension("toml.bak").exists());
    }

    fn ids_in(path: &Path, bot_id: Option<&str>) -> Vec<i64> {
        let raw = std::fs::read_to_string(path).unwrap();
        let doc: toml::Value = toml::from_str(&raw).unwrap();
        let section = if let Some(id) = bot_id {
            doc.get("bots")
                .and_then(|v| v.as_array())
                .and_then(|bots| {
                    bots.iter()
                        .find(|bot| bot.get("id").and_then(|v| v.as_str()) == Some(id))
                })
                .unwrap()
        } else {
            doc.get("telegram").unwrap()
        };
        section
            .get("allowed_user_ids")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_integer().unwrap())
            .collect()
    }

    #[test]
    fn first_sender_replaces_unowned_sentinel_and_second_does_not() {
        let dir = tempdir().unwrap();
        let path = write_cfg(
            &dir,
            r#"
[telegram]
bot_token = "123:abc"
allowed_user_ids = [0]
[openrouter]
api_key = "k"
model = "moonshotai/kimi-k2.6"
"#,
        );
        assert_eq!(
            claim_unowned_bot(&path, "default", 4242).unwrap(),
            UnownedClaim::Claimed
        );
        assert_eq!(ids_in(&path, None), vec![4242]);
        let before = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            claim_unowned_bot(&path, "default", 9999).unwrap(),
            UnownedClaim::Rejected
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        assert_eq!(
            claim_unowned_bot(&path, "default", 4242).unwrap(),
            UnownedClaim::Unchanged
        );
        assert_eq!(ids_in(&path, None), vec![4242]);
        // Sender 0 is the sentinel, never an owner.
        assert_eq!(
            claim_unowned_bot(&path, "other", 7).unwrap(),
            UnownedClaim::Unchanged
        );
    }

    #[test]
    fn existing_allowlist_is_not_overwritten() {
        let dir = tempdir().unwrap();
        let path = write_cfg(&dir, &minimal_toml());
        let before = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            claim_unowned_bot(&path, "default", 99).unwrap(),
            UnownedClaim::Rejected
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        assert_eq!(
            claim_unowned_bot(&path, "default", 42).unwrap(),
            UnownedClaim::Unchanged
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    }

    #[test]
    fn claim_updates_only_the_bot_that_received_the_message() {
        let dir = tempdir().unwrap();
        let path = write_cfg(
            &dir,
            r#"
[[bots]]
id = "alpha"
bot_token = "1:a"
allowed_user_ids = [0]
persona = "main"

[[bots]]
id = "beta"
bot_token = "2:b"
allowed_user_ids = [0]
persona = "main"

[openrouter]
api_key = "k"
model = "m"
"#,
        );
        assert_eq!(
            claim_unowned_bot(&path, "alpha", 55).unwrap(),
            UnownedClaim::Claimed
        );
        assert_eq!(ids_in(&path, Some("alpha")), vec![55]);
        assert_eq!(ids_in(&path, Some("beta")), vec![0]);
        assert_eq!(
            claim_unowned_bot(&path, "beta", 55).unwrap(),
            UnownedClaim::Claimed
        );
        assert_eq!(ids_in(&path, Some("beta")), vec![55]);
        assert_eq!(
            claim_unowned_bot(&path, "alpha", 77).unwrap(),
            UnownedClaim::Rejected
        );
        assert_eq!(ids_in(&path, Some("alpha")), vec![55]);
    }
}
