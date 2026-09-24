//! Skills / agents control plane (ADR 0011).
//!
//! Brings the previously read-only skill/agent surface under portal control:
//! detail + provenance, file editing, quarantine-style delete — reusing the
//! *same* name/path validators the agent's own tools use (`skill_tools`), so
//! there is exactly one traversal policy for both writers.
//!
//! Trust model: this endpoint group is granted to the same principal as the
//! agent's `write_skill_file` tool (token-authenticated owner, LAN/Tailnet
//! bind per ADR 0006/0007). The gates here prevent *accidents* (deleting a
//! bundled skill that would silently reappear, emptying a SKILL.md), not a
//! motivated owner.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use axum::extract::{Path as AxPath, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::error::PortalError;
use super::PortalState;
use crate::skill_tools::{validate_skill_name, validate_skill_path};
use crate::skills::seed::hash_skill_dir;

/// Max bytes per edited file (512 KB of markdown is already absurd).
const MAX_FILE_BYTES: usize = 512 * 1024;

// ---------------------------------------------------------------------------
// Provenance
// ---------------------------------------------------------------------------

/// Where a skill directory came from and whether it drifted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Provenance {
    /// Shipped inside the binary (`include_dir!`) — re-seeded/overwritten by
    /// the update engine. Editing is allowed; deletion is refused.
    Bundled,
    /// Installed from GitHub through this portal (`installed-skills.json`).
    Installed,
    /// Created by the user (agent tool, filesystem, or portal "new").
    User,
}

impl Provenance {
    pub fn as_str(&self) -> &'static str {
        match self {
            Provenance::Bundled => "bundled",
            Provenance::Installed => "installed",
            Provenance::User => "user",
        }
    }
}

/// One record in `installed-skills.json` (ADR 0011 C).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledRecord {
    pub source_repo: String,
    #[serde(default)]
    pub commit_sha: String,
    /// Git ref the tree was fetched at (branch or tag), recorded for humans.
    #[serde(default, rename = "ref")]
    pub git_ref: String,
    pub installed_at: String,
    /// `hash_skill_dir()` at install time — drift detection for installed skills.
    #[serde(default)]
    pub content_hash: String,
}

/// `~/.rustfox/installed-skills.json` — separate from `skills-lock.json`,
/// which is the *bundled* seed/update contract and must not be polluted.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct InstalledLedger {
    pub version: u32,
    #[serde(default)]
    pub skills: BTreeMap<String, InstalledRecord>,
    #[serde(default)]
    pub agents: BTreeMap<String, InstalledRecord>,
}

fn ledger_path(home: &Path) -> PathBuf {
    home.join("installed-skills.json")
}

pub fn read_ledger(home: &Path) -> InstalledLedger {
    std::fs::read_to_string(ledger_path(home))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(InstalledLedger {
            version: 1,
            skills: BTreeMap::new(),
            agents: BTreeMap::new(),
        })
}

pub fn write_ledger(home: &Path, ledger: &InstalledLedger) -> Result<(), PortalError> {
    let json = serde_json::to_string_pretty(ledger)
        .map_err(|e| PortalError::internal(format!("ledger serialize: {e}")))?;
    let path = ledger_path(home);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).map_err(PortalError::internal)?;
    std::fs::rename(&tmp, &path).map_err(PortalError::internal)?;
    Ok(())
}

/// Resolve a skill/agent name to its provenance.
/// Order: installed-ledger > bundled > user. Bundled check is *membership*,
/// not hash — an owner may have edited a bundled skill; it is still bundled.
pub fn classify(ledger_has: bool, is_bundled: bool) -> Provenance {
    if ledger_has {
        Provenance::Installed
    } else if is_bundled {
        Provenance::Bundled
    } else {
        Provenance::User
    }
}

/// Update-engine backups (`<name>.bak`) and portal quarantines
/// (`<name>.deleted-<ts>`) must never surface in listings — and the skills
/// loader applies the same predicate so they can never be *loaded* either
/// (ADR 0011a R3: one policy, two consumers).
pub use crate::skills::is_hidden_entry;

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

pub(crate) fn kind_root(state: &PortalState, kind: &str) -> PathBuf {
    let cfg = state.agent.config();
    if kind == "agents" {
        cfg.agents.directory.clone()
    } else {
        cfg.skills.directory.clone()
    }
}

pub(crate) fn dir_for(state: &PortalState, kind: &str, name: &str) -> PathBuf {
    kind_root(state, kind).join(name)
}

/// Directory-form primary file for a kind (skills: SKILL.md, agents: AGENT.md).
fn primary_name(kind: &str) -> &'static str {
    if kind == "agents" {
        "AGENT.md"
    } else {
        "SKILL.md"
    }
}

pub(crate) fn checked_name(name: &str) -> Result<(), PortalError> {
    validate_skill_name(name).map_err(|e| PortalError::bad_request("invalid_name", e))?;
    Ok(())
}

pub(crate) fn home_dir(state: &PortalState) -> Result<PathBuf, PortalError> {
    state
        .agent
        .config()
        .resolved_home
        .clone()
        .or_else(|| state.home_dir.clone())
        .ok_or_else(|| PortalError::internal("resolved_home unavailable for skills API"))
}

/// Locate the primary file for `(kind, name)`: either `dir/SKILL.md`,
/// `dir/AGENT.md`, or the loader-supported standalone `root/<name>.md`.
fn primary_path(state: &PortalState, kind: &str, name: &str) -> Option<PathBuf> {
    let dir = dir_for(state, kind, name);
    let p = dir.join(primary_name(kind));
    if p.is_file() {
        return Some(p);
    }
    let standalone = kind_root(state, kind).join(format!("{name}.md"));
    standalone.is_file().then_some(standalone)
}

/// The right lock map for a kind (skills-lock.json / agents-lock.json).
fn lock_map_for(kind: &str, home: &Path) -> BTreeMap<String, String> {
    if kind == "agents" {
        crate::skills::update::agents_lock_map_from_file(home)
    } else {
        crate::skills::update::lock_map_from_file(home)
    }
}

/// API-facing singular label ("skill" | "agent").
fn kind_label(kind: &str) -> &'static str {
    if kind == "agents" {
        "agent"
    } else {
        "skill"
    }
}

pub(crate) fn is_bundled(kind: &str, name: &str) -> bool {
    if kind == "agents" {
        crate::skills::embed::is_bundled_agent(name)
    } else {
        crate::skills::embed::is_bundled_skill(name)
    }
}

pub(crate) fn ledger_has(kind: &str, ledger: &InstalledLedger, name: &str) -> bool {
    if kind == "agents" {
        ledger.agents.contains_key(name)
    } else {
        ledger.skills.contains_key(name)
    }
}

/// If the markdown starts with YAML frontmatter, return its `name:` value.
pub(crate) fn frontmatter_name(content: &str) -> Option<String> {
    frontmatter_value(content, "name:")
}

fn frontmatter_value(content: &str, key: &str) -> Option<String> {
    let rest = content.trim_start().strip_prefix("---")?;
    let end = rest.find("\n---")?;
    for line in rest[..end].lines() {
        let line = line.trim_end();
        if let Some(v) = line.strip_prefix(key) {
            let v = v.trim().trim_matches(|c| c == '"' || c == '\'');
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// Extract the YAML flow/block list `tools: [a, b]` (loader-compatible).
fn frontmatter_list(content: &str, key: &str) -> Vec<String> {
    let Some(rest) = content.trim_start().strip_prefix("---") else {
        return vec![];
    };
    let Some(end) = rest.find("\n---") else {
        return vec![];
    };
    let mut out = Vec::new();
    let mut lines = rest[..end].lines().peekable();
    while let Some(line) = lines.next() {
        let t = line.trim_end();
        if let Some(v) = t.strip_prefix(key) {
            let v = v.trim();
            if v.starts_with('[') {
                for item in v.trim_start_matches('[').trim_end_matches(']').split(',') {
                    let item = item.trim().trim_matches(|c| c == '"' || c == '\'');
                    if !item.is_empty() {
                        out.push(item.to_string());
                    }
                }
                break;
            }
            // block sequence
            if v.is_empty() {
                while let Some(next) = lines.peek() {
                    let n = next.trim();
                    if let Some(item) = n.strip_prefix("- ") {
                        out.push(item.trim().to_string());
                        lines.next();
                    } else {
                        break;
                    }
                }
                break;
            }
        }
    }
    out
}

/// Content gates for the primary instruction file.
fn validate_primary_content(name: &str, file: &str, content: &str) -> Result<(), PortalError> {
    if content.trim().is_empty() {
        return Err(PortalError::bad_request(
            "empty_primary_file",
            format!("Refusing to write an empty {file} — delete the {name} instead"),
        ));
    }
    if let Some(fm) = frontmatter_name(content) {
        if fm != name {
            return Err(PortalError::bad_request(
                "frontmatter_name_mismatch",
                format!(
                    "frontmatter `name: {fm}` must match the directory name `{name}` \
                     (the loader keys skills by frontmatter name)"
                ),
            ));
        }
    }
    Ok(())
}

/// Reject symlink/`..` escapes even for hand-built requests: the parent of the
/// target (when it exists) must canonicalize under the skill directory.
fn ensure_within(dir: &Path, rel: &str) -> Result<PathBuf, PortalError> {
    validate_skill_path(rel).map_err(|e| PortalError::bad_request("invalid_path", e))?;
    if Path::new(rel).components().any(|c| {
        matches!(
            c,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(PortalError::bad_request(
            "invalid_path",
            "path escapes skill dir",
        ));
    }
    let target = dir.join(rel);
    if let Some(parent) = target.parent() {
        if parent.exists() {
            let canon_parent = parent.canonicalize().map_err(PortalError::internal)?;
            let canon_dir = dir.canonicalize().map_err(PortalError::internal)?;
            if !canon_parent.starts_with(&canon_dir) {
                return Err(PortalError::bad_request(
                    "invalid_path",
                    "symlink resolves outside the skill directory",
                ));
            }
        }
    }
    Ok(target)
}

/// Lowercase SHA-256 hex of a file, `None` when unreadable/missing —
/// doubles as the ETag for optimistic locking and the aux-file badge hash.
fn file_sha_hex(path: &Path) -> Option<String> {
    std::fs::read(path).ok().map(|b| {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(b);
        format!("{:x}", h.finalize())
    })
}

async fn read_if_exists(path: &Path) -> Result<Option<String>, PortalError> {
    match tokio::fs::read_to_string(path).await {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(PortalError::internal(e)),
    }
}

/// `.bak` + atomic tmp+rename (same pattern as settings/soul writes).
pub(crate) async fn write_with_backup(path: &Path, content: &str) -> Result<(), PortalError> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(PortalError::internal)?;
    }
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    if path.exists() {
        let bak = path.with_file_name(format!("{file_name}.bak"));
        tokio::fs::copy(path, &bak)
            .await
            .map_err(PortalError::internal)?;
    }
    let tmp = path.with_file_name(format!("{file_name}.tmp"));
    tokio::fs::write(&tmp, content)
        .await
        .map_err(PortalError::internal)?;
    tokio::fs::rename(&tmp, path)
        .await
        .map_err(PortalError::internal)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// GET /api/skills  ·  list with provenance (richer than /agents/skills)
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
pub struct ListQuery {
    #[serde(default)]
    pub kind: Option<String>,
}

async fn list_kind(state: &PortalState, kind: &'static str) -> Vec<Value> {
    let root = kind_root(state, kind);
    let Ok(mut rd) = tokio::fs::read_dir(&root).await else {
        return vec![];
    };
    let home = home_dir(state).ok();
    let ledger = home.as_ref().map(|h| read_ledger(h)).unwrap_or_default();
    let lock = home
        .as_ref()
        .map(|h| lock_map_for(kind, h))
        .unwrap_or_default();
    let mut out = Vec::new();
    while let Ok(Some(entry)) = rd.next_entry().await {
        let p = entry.path();
        let Some(os_name) = p.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let name = os_name.to_string();
        if is_hidden_entry(&name) {
            continue;
        }
        let display_name;
        let has_primary;
        if p.is_dir() {
            display_name = name.clone();
            has_primary = p.join(primary_name(kind)).is_file();
        } else if let Some(stem) = name.strip_suffix(".md") {
            // loader-supported standalone file skill
            display_name = stem.to_string();
            has_primary = stem != "README";
        } else {
            continue;
        };
        if !has_primary {
            continue;
        }
        let prov = classify(
            ledger_has(kind, &ledger, &display_name),
            is_bundled(kind, &display_name),
        );
        let hash = if p.is_dir() {
            hash_skill_dir(&p)
        } else {
            // standalone: hash just the file (same shape the detail view uses)
            file_sha_hex(&p)
        };
        let modified = match (&hash, lock.get(&display_name)) {
            (Some(h), Some(l)) => h != l,
            (Some(_), None) => prov == Provenance::User,
            (None, _) => false,
        };
        out.push(json!({
            "name": display_name,
            "kind": kind_label(kind),
            "provenance": prov.as_str(),
            "modified": modified,
            "deletable": prov != Provenance::Bundled,
        }));
    }
    out.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    out
}

pub async fn list_entries(
    State(state): State<PortalState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Value>, PortalError> {
    let want = q.kind.unwrap_or_else(|| "all".into());
    let mut out = Vec::new();
    match want.as_str() {
        "skills" => out.extend(list_kind(&state, "skills").await),
        "agents" => out.extend(list_kind(&state, "agents").await),
        _ => {
            out.extend(list_kind(&state, "skills").await);
            out.extend(list_kind(&state, "agents").await);
        }
    }
    Ok(Json(Value::Array(out)))
}

// ---------------------------------------------------------------------------
// GET /api/skills/{name} — detail
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct NameParams {
    pub name: String,
}

pub(crate) async fn detail_json(
    state: &PortalState,
    kind: &'static str,
    name: &str,
) -> Result<Json<Value>, PortalError> {
    checked_name(name)?;
    let Some(cp) = primary_path(state, kind, name) else {
        return Err(PortalError::not_found(kind));
    };
    let content = tokio::fs::read_to_string(&cp)
        .await
        .map_err(PortalError::internal)?;

    let home = home_dir(state)?;
    let ledger = read_ledger(&home);
    let lock = lock_map_for(kind, &home);
    let prov = classify(ledger_has(kind, &ledger, name), is_bundled(kind, name));
    let dir = dir_for(state, kind, name);
    let standalone = !dir.is_dir();
    let hash = if standalone {
        file_sha_hex(&cp)
    } else {
        hash_skill_dir(&dir)
    };
    // "modified" = drifted from whatever baseline we have on record.
    let baseline = lock.get(name).cloned().or_else(|| {
        if kind == "agents" {
            ledger.agents.get(name).map(|r| r.content_hash.clone())
        } else {
            ledger.skills.get(name).map(|r| r.content_hash.clone())
        }
    });
    let modified = match (&hash, &baseline) {
        (Some(h), Some(b)) => h != b,
        (Some(_), None) => prov == Provenance::User, // never recorded → user-created
        (None, _) => false,
    };

    let mut files = Vec::new();
    if !standalone {
        collect_files(&dir, &dir, &mut files)
            .await
            .map_err(PortalError::internal)?;
    }

    let rec = if kind == "agents" {
        ledger.agents.get(name)
    } else {
        ledger.skills.get(name)
    };
    let mut detail = json!({
        "name": name,
        "kind": kind_label(kind),
        "provenance": prov.as_str(),
        "deletable": prov != Provenance::Bundled,
        "modified": modified,
        "hash": hash,
        "standalone": standalone,
        "fileHash": file_sha_hex(&cp),
        "content": content,
        "files": files,
        "sourceRepo": rec.map(|r| r.source_repo.clone()),
        "commitSha": rec.map(|r| r.commit_sha.clone()),
        "installedAt": rec.map(|r| r.installed_at.clone()),
    });
    if kind == "agents" {
        detail["tools"] = json!(frontmatter_list(&content, "tools:"));
        detail["model"] = json!(frontmatter_value(&content, "model:"));
    }
    Ok(Json(detail))
}

async fn collect_files(root: &Path, dir: &Path, out: &mut Vec<Value>) -> std::io::Result<()> {
    let mut rd = tokio::fs::read_dir(dir).await?;
    while let Some(e) = rd.next_entry().await? {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if crate::skills::is_hidden_entry(&name) || name.ends_with(".tmp") {
            continue;
        }
        if p.is_dir() {
            Box::pin(collect_files(root, &p, out)).await?;
        } else if let Ok(md) = tokio::fs::metadata(&p).await {
            out.push(json!({
                "path": p.strip_prefix(root)
                    .unwrap_or(&p)
                    .to_string_lossy()
                    .replace('\\', "/"),
                "size": md.len(),
                "hash": file_sha_hex(&p),
            }));
        }
    }
    Ok(())
}

pub async fn entry_detail(
    State(state): State<PortalState>,
    AxPath(p): AxPath<NameParams>,
) -> Result<Json<Value>, PortalError> {
    detail_json(&state, "skills", &p.name).await
}

pub async fn agent_detail(
    State(state): State<PortalState>,
    AxPath(p): AxPath<NameParams>,
) -> Result<Json<Value>, PortalError> {
    detail_json(&state, "agents", &p.name).await
}

// ---------------------------------------------------------------------------
// GET/PUT /api/skills/{name}/file[?path=]
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct FileQuery {
    pub path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteQuery {
    /// Escape hatch (agents): save despite tools that don't exist at runtime.
    /// Accepts `1`/`true` (serde_urlencoded parses bools literally, so a
    /// plain `bool` field would 400 on `?allowMissing=1`).
    #[serde(default, deserialize_with = "from_truthy")]
    pub allow_missing: bool,
}

/// Lenient bool for query strings: absent/""/0/false → false; anything else
/// truthy-ish (`1`, `true`, `yes`) → true. Unknown junk → error, so typos
/// don't silently enable the escape hatch.
fn from_truthy<'de, D>(d: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(d).ok();
    match s.as_deref() {
        None | Some("") | Some("0") | Some("false") | Some("no") => Ok(false),
        Some("1") | Some("true") | Some("yes") => Ok(true),
        Some(other) => Err(serde::de::Error::custom(format!(
            "invalid boolean '{other}'"
        ))),
    }
}

/// Resolve a request path to a file on disk. Directory-form skills resolve
/// under `dir/`; standalone `<name>.md` skills only expose their own file
/// (addressed by the primary name, e.g. `SKILL.md`).
fn resolve_target(
    state: &PortalState,
    kind: &str,
    name: &str,
    rel: &str,
) -> Result<PathBuf, PortalError> {
    let dir = dir_for(state, kind, name);
    if dir.is_dir() {
        return ensure_within(&dir, rel);
    }
    let standalone = kind_root(state, kind).join(format!("{name}.md"));
    if standalone.is_file() {
        if rel == primary_name(kind) {
            return Ok(standalone);
        }
        return Err(PortalError::bad_request(
            "invalid_path",
            "standalone .md skills only expose their primary file",
        ));
    }
    // Neither form exists. `create_entry_kind` pre-makes the dir and calls
    // `write_new_primary` directly, so this branch only serves that internal
    // flow — the public PUT gate 404s before reaching here (ADR 0011a R5:
    // creation is a separate intent with its own endpoint).
    if kind_root(state, kind).is_dir() {
        return ensure_within(&dir, rel);
    }
    Err(PortalError::not_found(kind))
}

async fn read_file_kind(
    state: &PortalState,
    kind: &str,
    name: &str,
    rel: &str,
) -> Result<Json<Value>, PortalError> {
    checked_name(name)?;
    let target = resolve_target(state, kind, name, rel)?;
    let content = read_if_exists(&target)
        .await?
        .ok_or_else(|| PortalError::not_found("file"))?;
    Ok(Json(
        json!({ "path": rel, "content": content, "size": content.len() }),
    ))
}

pub async fn read_file(
    State(state): State<PortalState>,
    AxPath(p): AxPath<NameParams>,
    Query(q): Query<FileQuery>,
) -> Result<Json<Value>, PortalError> {
    read_file_kind(&state, "skills", &p.name, &q.path).await
}

pub async fn read_agent_file(
    State(state): State<PortalState>,
    AxPath(p): AxPath<NameParams>,
    Query(q): Query<FileQuery>,
) -> Result<Json<Value>, PortalError> {
    read_file_kind(&state, "agents", &p.name, &q.path).await
}

/// Body for `PUT .../file` (update-only) — plus optional optimistic-lock
/// token: `baseHash` from the last `GET detail` (ADR 0011a R4).
#[derive(Deserialize)]
pub struct FileWrite {
    pub path: String,
    pub content: String,
    /// SHA-256 the client last read for this file (detail `hash` for the
    /// primary, `files[].hash` for aux). Absent → force-write (CLI parity).
    #[serde(default, rename = "baseHash")]
    pub base_hash: Option<String>,
}

fn check_base_hash(target: &Path, base_hash: &Option<String>) -> Result<(), PortalError> {
    let Some(expect) = base_hash.as_deref().map(str::trim) else {
        return Ok(());
    };
    if expect.is_empty() {
        return Ok(());
    }
    let Some(current) = file_sha_hex(target) else {
        // File vanished since the client read it — that IS a change.
        return Err(PortalError::conflict(
            "changed_since_read",
            "file was removed since you read it; re-read and retry",
        ));
    };
    if current != expect {
        return Err(PortalError::conflict(
            "changed_since_read",
            format!(
                "file changed since you read it (server {current}\u{2026}, you sent \
                     {expect}\u{2026}) — reload the editor and merge your edit"
            ),
        ));
    }
    Ok(())
}

/// Shared write pipeline for skills and agents (kind-selected gates).
async fn write_file_kind(
    state: &PortalState,
    kind: &'static str,
    name: &str,
    body: &FileWrite,
    allow_missing: bool,
) -> Result<(bool, Provenance, Vec<String>), PortalError> {
    checked_name(name)?;
    if body.content.len() > MAX_FILE_BYTES {
        return Err(PortalError::bad_request(
            "file_too_large",
            format!("max {MAX_FILE_BYTES} bytes per file"),
        ));
    }
    // PUT is update-only (ADR 0011a R5): creating an entry is its own intent,
    // POST /api/{kind}. resolve_target no longer fabricates fresh dirs.
    if primary_path(state, kind, name).is_none() {
        return Err(PortalError::new(
            axum::http::StatusCode::NOT_FOUND,
            "not_found",
            format!("{kind} `{name}` does not exist — create it via POST"),
        ));
    }
    let target = resolve_target(state, kind, name, &body.path)?;
    let primary = primary_name(kind);
    let is_primary = body.path == primary;

    check_base_hash(&target, &body.base_hash)?;

    if is_primary {
        validate_primary_content(name, primary, &body.content)?;
    }

    // Agents: every declared tool must exist at runtime (ADR 0011 / RRSI
    // evidence — a tool list referencing a missing MCP server is dead weight
    // in the system prompt). Hard gate at save time, ?allowMissing=1 escape.
    let mut missing_tools: Vec<String> = Vec::new();
    if kind == "agents" && is_primary {
        let declared = frontmatter_list(&body.content, "tools:");
        let available: Vec<String> = state.agent.tool_names();
        missing_tools = declared
            .iter()
            .filter(|t| !available.iter().any(|a| a == *t))
            .cloned()
            .collect();
        if !missing_tools.is_empty() && !allow_missing {
            return Err(PortalError::bad_request(
                "unknown_tools",
                format!(
                    "tools not available at runtime: {} — configure the server or \
                     save with ?allowMissing=1 to keep them anyway",
                    missing_tools.join(", ")
                ),
            ));
        }
    }

    write_with_backup(&target, &body.content).await?;
    let home = home_dir(state)?;
    let ledger = read_ledger(&home);
    let prov = classify(ledger_has(kind, &ledger, name), is_bundled(kind, name));
    tracing::info!("Portal: wrote {kind}/{name}/{} ({prov:?})", body.path);
    if !missing_tools.is_empty() {
        tracing::warn!("Portal: {kind} `{name}` saved with unknown tools: {missing_tools:?}");
    }
    // Primary edits change what the registry holds → reload; aux-only edits
    // don't (SKILL.md is the only file the loader reads at boot).
    let reload = is_primary;
    let warnings = if missing_tools.is_empty() {
        Vec::new()
    } else {
        vec![format!(
            "unknown tools declared: {} (saved anyway via allowMissing)",
            missing_tools.join(", ")
        )]
    };
    Ok((reload, prov, warnings))
}

pub async fn write_file(
    State(state): State<PortalState>,
    AxPath(p): AxPath<NameParams>,
    Query(q): Query<WriteQuery>,
    Json(body): Json<FileWrite>,
) -> Result<Json<Value>, PortalError> {
    let (reload, prov, warnings) =
        write_file_kind(&state, "skills", &p.name, &body, q.allow_missing).await?;
    let counts = if reload {
        state.agent.reload_skills_and_agents().await
    } else {
        (0, 0)
    };
    Ok(Json(json!({
        "ok": true, "name": p.name, "path": body.path,
        "bytes": body.content.len(), "provenance": prov.as_str(),
        "skillsLoaded": counts.0, "warnings": warnings,
    })))
}

pub async fn write_agent_file(
    State(state): State<PortalState>,
    AxPath(p): AxPath<NameParams>,
    Query(q): Query<WriteQuery>,
    Json(body): Json<FileWrite>,
) -> Result<Json<Value>, PortalError> {
    let (reload, prov, warnings) =
        write_file_kind(&state, "agents", &p.name, &body, q.allow_missing).await?;
    let counts = if reload {
        state.agent.reload_skills_and_agents().await
    } else {
        (0, 0)
    };
    Ok(Json(json!({
        "ok": true, "name": p.name, "path": body.path,
        "bytes": body.content.len(), "provenance": prov.as_str(),
        "agentsLoaded": counts.1, "warnings": warnings,
    })))
}

// ---------------------------------------------------------------------------
// POST /api/skills · POST /api/agents — structured create (ADR 0011a R5)
// ---------------------------------------------------------------------------

/// Create body. For skills, `content` is required (the SKILL.md). For
/// agents, the frontmatter is *rendered* from structured fields so a
/// typo'd YAML key can never brick the loader; `content` carries the body
/// instructions below the frontmatter.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateEntry {
    pub name: String,
    /// Full primary-file content (skills) or body-only (agents).
    #[serde(default)]
    pub content: String,
    // ---- agent-only structured fields ----
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub tools: Option<Vec<String>>,
    #[serde(default)]
    pub max_iterations: Option<u32>,
    #[serde(default)]
    pub skip_bootstrap: Option<bool>,
}

fn yaml_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Render an AGENT.md from structured fields + body.
fn render_agent_md(
    name: &str,
    description: &str,
    model: Option<&str>,
    tools: &[String],
    max_iterations: Option<u32>,
    skip_bootstrap: Option<bool>,
    body: &str,
) -> String {
    let mut fm = String::from("---\n");
    fm.push_str(&format!("name: {name}\n"));
    fm.push_str(&format!("description: {}\n", yaml_quote(description)));
    if let Some(m) = model {
        if !m.trim().is_empty() {
            fm.push_str(&format!("model: {m}\n"));
        }
    }
    if !tools.is_empty() {
        fm.push_str("tools:\n");
        for t in tools {
            fm.push_str(&format!("  - {}\n", t.trim()));
        }
    }
    if let Some(mi) = max_iterations {
        fm.push_str(&format!("max_iterations: {mi}\n"));
    }
    if matches!(skip_bootstrap, Some(true)) {
        fm.push_str("skip_bootstrap: true\n");
    }
    fm.push_str("---\n");
    fm.push_str(body.trim_start());
    fm
}

/// Same gates as `write_file_kind` but for a brand-new entry (no ETag check,
/// no update-only 404). Shares the content/tool gates by construction.
async fn write_new_primary(
    state: &PortalState,
    kind: &'static str,
    name: &str,
    path: &str,
    content: &str,
    allow_missing: bool,
) -> Result<(Provenance, Vec<String>), PortalError> {
    if content.len() > MAX_FILE_BYTES {
        return Err(PortalError::bad_request(
            "file_too_large",
            format!("max {MAX_FILE_BYTES} bytes per file"),
        ));
    }
    let target = resolve_target(state, kind, name, path)?;
    let primary = primary_name(kind);
    validate_primary_content(name, primary, content)?;

    let mut missing_tools: Vec<String> = Vec::new();
    if kind == "agents" {
        let declared = frontmatter_list(content, "tools:");
        let available: Vec<String> = state.agent.tool_names();
        missing_tools = declared
            .iter()
            .filter(|t| !available.iter().any(|a| a == *t))
            .cloned()
            .collect();
        if !missing_tools.is_empty() && !allow_missing {
            return Err(PortalError::bad_request(
                "unknown_tools",
                format!(
                    "tools not available at runtime: {} — configure the server or \
                     POST with ?allowMissing=1 to keep them anyway",
                    missing_tools.join(", ")
                ),
            ));
        }
    }
    write_with_backup(&target, content).await?;
    let home = home_dir(state)?;
    let ledger = read_ledger(&home);
    let prov = classify(ledger_has(kind, &ledger, name), is_bundled(kind, name));
    let warnings = if missing_tools.is_empty() {
        Vec::new()
    } else {
        vec![format!(
            "unknown tools declared: {} (saved anyway via allowMissing)",
            missing_tools.join(", ")
        )]
    };
    Ok((prov, warnings))
}

async fn create_entry_kind(
    state: &PortalState,
    kind: &'static str,
    body: CreateEntry,
    allow_missing: bool,
) -> Result<Json<Value>, PortalError> {
    checked_name(&body.name)?;
    let primary = primary_name(kind);
    let content = if kind == "agents" {
        let desc = body
            .description
            .clone()
            .filter(|d| !d.trim().is_empty())
            .unwrap_or_else(|| format!("Subagent `{}`", body.name));
        render_agent_md(
            &body.name,
            &desc,
            body.model.as_deref(),
            &body.tools.clone().unwrap_or_default(),
            body.max_iterations,
            body.skip_bootstrap,
            &body.content,
        )
    } else {
        body.content.clone()
    };
    if primary_path(state, kind, &body.name).is_some() {
        return Err(PortalError::conflict(
            "already_exists",
            format!(
                "{} `{}` already exists — edit it via PUT",
                kind_label(kind),
                body.name
            ),
        ));
    }
    if !kind_root(state, kind).is_dir() {
        return Err(PortalError::not_found(kind));
    }
    let dir = dir_for(state, kind, &body.name);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(PortalError::internal)?;
    // Roll the fresh dir back if a gate below refuses the content.
    let outcome =
        write_new_primary(state, kind, &body.name, primary, &content, allow_missing).await;
    let (prov, warnings) = match outcome {
        Ok(v) => v,
        Err(e) => {
            let _ = tokio::fs::remove_dir_all(&dir).await;
            return Err(e);
        }
    };
    let counts = state.agent.reload_skills_and_agents().await;
    tracing::info!("Portal: created {kind} `{}` ({prov:?})", body.name);
    Ok(Json(json!({
        "ok": true,
        "name": body.name,
        "kind": kind_label(kind),
        "path": primary,
        "bytes": content.len(),
        "provenance": prov.as_str(),
        "skillsLoaded": counts.0,
        "agentsLoaded": counts.1,
        "warnings": warnings,
    })))
}

pub async fn create_skill(
    State(state): State<PortalState>,
    Query(q): Query<WriteQuery>,
    Json(body): Json<CreateEntry>,
) -> Result<Json<Value>, PortalError> {
    create_entry_kind(&state, "skills", body, q.allow_missing).await
}

pub async fn create_agent(
    State(state): State<PortalState>,
    Query(q): Query<WriteQuery>,
    Json(body): Json<CreateEntry>,
) -> Result<Json<Value>, PortalError> {
    create_entry_kind(&state, "agents", body, q.allow_missing).await
}

// ---------------------------------------------------------------------------
// DELETE — quarantine, never hard-remove; bundled refused (ADR 0011 A)
// ---------------------------------------------------------------------------

pub(crate) async fn quarantine(
    state: &PortalState,
    kind: &'static str,
    name: &str,
) -> Result<Json<Value>, PortalError> {
    checked_name(name)?;
    if is_bundled(kind, name) {
        return Err(PortalError::new(
            axum::http::StatusCode::FORBIDDEN,
            "bundled_readonly",
            format!(
                "`{name}` ships inside the RustFox binary — deleting it here would \
                     be undone by the next update. Fork it under a new name instead \
                     (GET content → PUT to a new {kind} name)."
            ),
        ));
    }
    let dir = dir_for(state, kind, name);
    let standalone = kind_root(state, kind).join(format!("{name}.md"));
    let stamp = chrono::Utc::now().format("%Y%m%d%H%M%S");
    // Structural quarantine (ADR 0011a R3): move into `<root>/.trash/`, a
    // dot-dir the top-level loader scan can never mistake for an entry —
    // no string matching required to stay inert. The loader filter is layer 2.
    let trash = kind_root(state, kind).join(".trash");
    tokio::fs::create_dir_all(&trash)
        .await
        .map_err(PortalError::internal)?;
    if dir.is_dir() {
        let quarantined = trash.join(format!("{name}-{stamp}"));
        tokio::fs::rename(&dir, &quarantined)
            .await
            .map_err(PortalError::internal)?;
        finish_quarantine(state, kind, name, quarantined).await
    } else if standalone.is_file() {
        let quarantined = trash.join(format!("{name}-{stamp}.md"));
        tokio::fs::rename(&standalone, &quarantined)
            .await
            .map_err(PortalError::internal)?;
        finish_quarantine(state, kind, name, quarantined).await
    } else {
        Err(PortalError::not_found(kind))
    }
}

async fn finish_quarantine(
    state: &PortalState,
    kind: &str,
    name: &str,
    to: PathBuf,
) -> Result<Json<Value>, PortalError> {
    let home = home_dir(state)?;
    let mut ledger = read_ledger(&home);
    let had = if kind == "agents" {
        ledger.agents.remove(name).is_some()
    } else {
        ledger.skills.remove(name).is_some()
    };
    if had {
        write_ledger(&home, &ledger)?;
    }
    let (s, a) = state.agent.reload_skills_and_agents().await;
    tracing::info!("Portal: quarantined {kind} `{name}` → {}", to.display());
    Ok(Json(json!({
        "ok": true, "name": name, "kind": kind_label(kind),
        "quarantinedTo": to.file_name().and_then(|n| n.to_str()),
        "skillsLoaded": s, "agentsLoaded": a,
    })))
}

pub async fn delete_entry(
    State(state): State<PortalState>,
    AxPath(p): AxPath<NameParams>,
) -> Result<Json<Value>, PortalError> {
    quarantine(&state, "skills", &p.name).await
}

pub async fn delete_agent(
    State(state): State<PortalState>,
    AxPath(p): AxPath<NameParams>,
) -> Result<Json<Value>, PortalError> {
    quarantine(&state, "agents", &p.name).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provenance_order_prefers_ledger() {
        assert_eq!(classify(true, true), Provenance::Installed);
        assert_eq!(classify(false, true), Provenance::Bundled);
        assert_eq!(classify(false, false), Provenance::User);
    }

    #[test]
    fn hidden_entries_cover_backups_and_quarantine() {
        assert!(is_hidden_entry("my-skill.bak"));
        assert!(is_hidden_entry("my-skill.deleted-20260925010101"));
        assert!(is_hidden_entry(".DS_Store"));
        assert!(!is_hidden_entry("my-skill"));
    }

    #[test]
    fn frontmatter_name_extracted_when_present() {
        let c = "---\nname: alpha\ndescription: hi\n---\n# body";
        assert_eq!(frontmatter_name(c).as_deref(), Some("alpha"));
    }

    #[test]
    fn frontmatter_name_none_without_block_or_key() {
        assert_eq!(frontmatter_name("# no fm"), None);
        assert_eq!(frontmatter_name("---\ndescription: x\n---\nbody"), None);
    }

    #[test]
    fn frontmatter_list_flow_and_block_forms() {
        let flow = "---\nname: a\ntools: [read_file, write_file]\n---\nbody";
        assert_eq!(
            frontmatter_list(flow, "tools:"),
            vec!["read_file", "write_file"]
        );
        let block = "---\nname: a\ntools:\n  - read_file\n  - execute_command\nmax_iterations: 5\n---\nbody";
        assert_eq!(
            frontmatter_list(block, "tools:"),
            vec!["read_file", "execute_command"]
        );
        assert!(frontmatter_list("---\nname: a\n---\n", "tools:").is_empty());
    }

    #[test]
    fn empty_primary_file_refused() {
        let e = validate_primary_content("alpha", "SKILL.md", "   \n").unwrap_err();
        assert_eq!(e.code, "empty_primary_file");
    }

    #[test]
    fn mismatched_frontmatter_name_refused() {
        let e = validate_primary_content("alpha", "SKILL.md", "---\nname: beta\n---\nbody")
            .unwrap_err();
        assert_eq!(e.code, "frontmatter_name_mismatch");
        // The message must name the dir (the loader keys by frontmatter name —
        // silently renaming via the editor would orphan the skill.)
        assert!(e.message.contains("alpha") && e.message.contains("beta"));
    }

    #[test]
    fn matching_or_missing_frontmatter_accepted() {
        assert!(
            validate_primary_content("alpha", "SKILL.md", "---\nname: alpha\n---\nbody").is_ok()
        );
        assert!(validate_primary_content("alpha", "SKILL.md", "# plain, no frontmatter").is_ok());
    }

    #[test]
    fn ensure_within_blocks_traversal_and_absolute() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(ensure_within(tmp.path(), "../escape").is_err());
        assert!(ensure_within(tmp.path(), "/etc/passwd").is_err());
        assert!(ensure_within(tmp.path(), "scripts/ok.md").is_ok());
    }

    #[test]
    fn ensure_within_blocks_symlink_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("skill");
        std::fs::create_dir_all(&dir).unwrap();
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, dir.join("link")).unwrap();
            let err = ensure_within(&dir, "link/evil.md").unwrap_err();
            assert_eq!(err.code, "invalid_path");
        }
    }

    #[test]
    fn write_query_accepts_lenient_bools() {
        let q: WriteQuery = serde_urlencoded::from_str("allowMissing=1").unwrap();
        assert!(q.allow_missing);
        let q: WriteQuery = serde_urlencoded::from_str("allowMissing=true").unwrap();
        assert!(q.allow_missing);
        let q: WriteQuery = serde_urlencoded::from_str("allowMissing=0").unwrap();
        assert!(!q.allow_missing);
        let q: WriteQuery = serde_urlencoded::from_str("").unwrap();
        assert!(!q.allow_missing);
        // junk must NOT enable the escape hatch silently
        assert!(serde_urlencoded::from_str::<WriteQuery>("allowMissing=maybe").is_err());
    }

    #[test]
    fn kind_labels_are_singular() {
        assert_eq!(kind_label("skills"), "skill");
        assert_eq!(kind_label("agents"), "agent");
    }

    #[test]
    fn primary_name_per_kind() {
        assert_eq!(primary_name("skills"), "SKILL.md");
        assert_eq!(primary_name("agents"), "AGENT.md");
    }
}
