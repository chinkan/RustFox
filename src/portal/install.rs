//! Native GitHub skill installer (ADR 0011 B / 0011a R2, R3, R8).
//!
//! Two-phase: `dryRun:true` returns the plan + safety verdicts with zero
//! writes; `dryRun:false` installs only after every warning from the dry run
//! has been explicitly acknowledged (RRSI leak-screening: reject *before* it
//! scores — but the owner can override warnings, never miss them).
//!
//! Hard refusals (no override): secret-shaped content, executable-extension
//! files, bundled-name collisions, caps breaches. These are supply-chain
//! foot-gun reducers, not a sandbox (ADR 0011 D).

use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::control::{self, InstalledRecord};
use super::error::PortalError;
use super::PortalState;
use crate::skills::seed::hash_skill_dir;

// ---------------------------------------------------------------------------
// Caps (ADR 0011 B)
// ---------------------------------------------------------------------------

pub const MAX_SKILLS_PER_REQUEST: usize = 10;
pub const MAX_FILES_PER_SKILL: usize = 20;
pub const MAX_FILE_BYTES: usize = 512 * 1024;
pub const MAX_SKILL_BYTES: usize = 2 * 1024 * 1024;
/// SKILL.md may sit no deeper than this below the repo root (or subpath).
pub const MAX_SKILL_DEPTH: usize = 3;

const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "dist",
    "__pycache__",
    ".venv",
    "target",
];

/// Extensions that mark a file as executable code — never installed (the
/// installer cannot run anything, and a skill that needs code must be
/// installed manually with eyes open).
pub const EXECUTABLE_EXTS: &[&str] = &[
    ".sh", ".py", ".js", ".ts", ".pl", ".rb", ".exe", ".dll", ".dylib", ".so", ".bin", ".ps1",
];

// ---------------------------------------------------------------------------
// Fetcher trait — tests fake the network
// ---------------------------------------------------------------------------

#[async_trait]
pub trait GitHubFetcher: Send + Sync {
    /// GET a URL expecting JSON. Returns parsed Value or Err(String).
    async fn get_json(&self, url: &str) -> Result<Value, String>;
    /// GET a URL expecting raw bytes.
    async fn get_bytes(&self, url: &str) -> Result<Vec<u8>, String>;
}

/// Production fetcher: 30 s timeout, honest User-Agent, no redirects-to-file.
pub struct ReqwestFetcher {
    client: reqwest::Client,
}

impl ReqwestFetcher {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .user_agent(concat!("rustfox-portal/", env!("CARGO_PKG_VERSION")))
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("reqwest client"),
        }
    }
}

impl Default for ReqwestFetcher {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl GitHubFetcher for ReqwestFetcher {
    async fn get_json(&self, url: &str) -> Result<Value, String> {
        let res = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = res.status();
        if status == reqwest::StatusCode::FORBIDDEN
            || status == reqwest::StatusCode::TOO_MANY_REQUESTS
        {
            return Err(format!(
                "github_rate_limited: {status} — wait a few minutes or install manually"
            ));
        }
        if !status.is_success() {
            return Err(format!("github returned {status}"));
        }
        res.json::<Value>().await.map_err(|e| e.to_string())
    }
    async fn get_bytes(&self, url: &str) -> Result<Vec<u8>, String> {
        let res = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = res.status();
        if !status.is_success() {
            return Err(format!("raw.githubusercontent returned {status}"));
        }
        Ok(res.bytes().await.map_err(|e| e.to_string())?.to_vec())
    }
}

// ---------------------------------------------------------------------------
// Source spec: owner/repo[:subpath]@ref
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceSpec {
    pub owner: String,
    pub repo: String,
    /// Optional subdirectory the search is rooted at (monorepos).
    pub subpath: Option<String>,
    /// Explicit branch/tag/sha; None → repo default branch.
    pub git_ref: Option<String>,
}

fn valid_token(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

pub fn parse_source(raw: &str) -> Result<SourceSpec, PortalError> {
    let raw = raw.trim();
    if raw.contains("..") {
        return Err(PortalError::bad_request(
            "invalid_source",
            "source must not contain '..'",
        ));
    }
    // strip https://github.com/ prefix if pasted from a browser
    let raw = raw
        .strip_prefix("https://github.com/")
        .or_else(|| raw.strip_prefix("github.com/"))
        .unwrap_or(raw);
    let (body, git_ref) = match raw.split_once('@') {
        Some((b, r)) => {
            if r.is_empty() {
                return Err(PortalError::bad_request(
                    "invalid_source",
                    "empty ref after '@'",
                ));
            }
            (b, Some(r.to_string()))
        }
        None => (raw, None),
    };
    let (repo_part, subpath) = match body.split_once(':') {
        Some((r, sp)) => (r, Some(sp.trim_matches('/').to_string())),
        None => (body, None),
    };
    let mut segs = repo_part.splitn(2, '/');
    let owner = segs.next().unwrap_or("");
    let repo = segs.next().unwrap_or("");
    if segs.next().is_some() || !valid_token(owner) || !valid_token(repo) {
        return Err(PortalError::bad_request(
            "invalid_source",
            "expected owner/repo[:subpath][@ref] with [A-Za-z0-9._-] tokens",
        ));
    }
    if let Some(sp) = &subpath {
        if sp.is_empty()
            || sp.split('/').any(|c| c.is_empty() || c == "..")
            || !sp
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | ' '))
        {
            return Err(PortalError::bad_request(
                "invalid_source",
                "subpath must be a clean relative path",
            ));
        }
    }
    if let Some(r) = &git_ref {
        if r.contains("..")
            || !r
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'))
        {
            return Err(PortalError::bad_request("invalid_source", "bad ref"));
        }
    }
    Ok(SourceSpec {
        owner: owner.to_string(),
        repo: repo.to_string(),
        subpath,
        git_ref,
    })
}

// ---------------------------------------------------------------------------
// Scanners — the RRSI pre-flight gauntlet
// ---------------------------------------------------------------------------

/// Refuse-level finding: file will not be installed, no override.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Refusal {
    pub skill: String,
    pub file: String,
    pub rule: String,
    pub detail: String,
}

/// Warning-level finding: installable only with explicit acknowledgement.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Warning {
    pub skill: String,
    pub file: String,
    pub rule: String,
    pub detail: String,
}

/// Secret shapes — same families as the 9/21 credential incident. A leaked
/// credential inside a skill is a live system-prompt injection vector.
pub fn secret_findings(text: &str) -> Vec<(&'static str, String)> {
    let mut hits = Vec::new();
    for needle in [
        "GOCSPX-",
        "AIza",
        "AKIA",
        "xoxb-",
        "xoxp-",
        "xoxa-",
        "-----BEGIN",
    ] {
        if let Some(pos) = text.find(needle) {
            hits.push((
                "secret_shape",
                format!("contains {needle}… at offset {pos}"),
            ));
        }
    }
    // sk-<20+ alnum> and Google refresh tokens 1//<20+>
    for (prefix, name) in [("sk-", "api_key_shape"), ("1//", "oauth_refresh_shape")] {
        for (idx, _) in text.match_indices(prefix) {
            let tail = &text[idx + prefix.len()..];
            // maximal key-charset run right after the prefix: prose like
            // "the sk- prefix" has a run of 0 (space), a real key is 20+
            // unbroken key chars.
            let run = tail
                .char_indices()
                .take_while(|(_, c)| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
                .last()
                .map(|(i, c)| i + c.len_utf8())
                .unwrap_or(0);
            if run >= 20 {
                hits.push((name, format!("{prefix}<…{run} chars…> at offset {idx}")));
            }
        }
    }
    hits
}

/// Injection-shape heuristics → warnings, not refusals (ADR 0011a R2).
pub fn injection_findings(text: &str) -> Vec<(&'static str, String)> {
    let mut hits = Vec::new();
    let lower = text.to_lowercase();
    let phrases = [
        "ignore previous instructions",
        "ignore all previous",
        "disregard the system prompt",
        "you must override",
        "do not tell the user",
        "when nobody is looking",
        "override your safety",
        "reveal your system prompt",
    ];
    for p in phrases {
        if lower.contains(p) {
            hits.push(("injection_phrase", p.to_string()));
        }
    }
    // invisible unicode (zero-width / bidi overrides) outside code fences is
    // a classic prompt-injection cloak
    for (idx, ch) in text.char_indices() {
        if matches!(
            ch,
            '\u{200B}'
                | '\u{200C}'
                | '\u{200D}'
                | '\u{2060}'
                | '\u{202E}'
                | '\u{202D}'
                | '\u{FEFF}'
        ) {
            hits.push(("invisible_unicode", format!("U+{:04X} at {idx}", ch as u32)));
            break; // one report is enough
        }
    }
    // hidden HTML comments
    let mut src = text;
    while let Some(a) = src.find("<!--") {
        let rest = &src[a..];
        let Some(b) = rest.find("-->") else { break };
        let inner = rest[4..b.max(4)].to_lowercase();
        if inner.contains("ignore") || inner.contains("instruction") || inner.contains("system") {
            hits.push((
                "hidden_comment",
                "HTML comment carrying directive words".into(),
            ));
            break;
        }
        src = &rest[b + 3..];
    }
    hits
}

pub fn has_executable_ext(path: &str) -> bool {
    let p = path.to_lowercase();
    EXECUTABLE_EXTS.iter().any(|e| p.ends_with(e))
}

// ---------------------------------------------------------------------------
// Discovery: tree walk → skill candidates
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct DiscoveredFile {
    pub path: String,     // relative to the skill dir
    pub sha_path: String, // full repo path for raw fetch
    pub size: u64,
}

#[derive(Debug, Clone)]
pub struct DiscoveredSkill {
    pub name: String,
    pub dir_path: String, // repo-relative dir
    pub skill_md_path: String,
    pub files: Vec<DiscoveredFile>,
    pub total_bytes: u64,
}

fn tree_paths(tree: &Value) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    if let Some(arr) = tree.get("tree").and_then(|t| t.as_array()) {
        for item in arr {
            if item.get("type").and_then(Value::as_str) != Some("blob") {
                continue;
            }
            let Some(path) = item.get("path").and_then(Value::as_str) else {
                continue;
            };
            let size = item.get("size").and_then(Value::as_u64).unwrap_or(0);
            out.push((path.to_string(), size));
        }
    }
    out
}

/// Find skill dirs in a (possibly subpath-filtered) recursive tree listing.
pub fn discover_skills(spec: &SourceSpec, tree: &Value) -> Vec<DiscoveredSkill> {
    let prefix = spec
        .subpath
        .as_ref()
        .map(|s| {
            if s.is_empty() {
                String::new()
            } else {
                format!("{s}/")
            }
        })
        .unwrap_or_default();
    let mut by_dir: std::collections::BTreeMap<String, Vec<(String, u64)>> = Default::default();
    for (path, size) in tree_paths(tree) {
        let Some(rel) = path.strip_prefix(&prefix) else {
            continue;
        };
        let segs: Vec<&str> = rel.split('/').collect();
        if segs.len() < 2 || segs.len() - 1 > MAX_SKILL_DEPTH {
            continue; // SKILL.md must be inside at least one dir, within depth
        }
        if segs
            .iter()
            .any(|s| SKIP_DIRS.contains(s) || s.starts_with('.'))
        {
            continue;
        }
        if segs.last() != Some(&"SKILL.md") {
            continue;
        }
        let dir = segs[..segs.len() - 1].join("/");
        by_dir.entry(dir).or_default().push((rel.to_string(), size));
    }
    let mut out = Vec::new();
    for (dir, _entries) in by_dir {
        let name = dir.rsplit('/').next().unwrap_or(&dir).to_string();
        if crate::skill_tools::validate_skill_name(&name).is_err() {
            continue;
        }
        // gather ALL files under this dir (any depth), not just SKILL.md
        let mut all: Vec<(String, u64)> = Vec::new();
        for (path, size) in tree_paths(tree) {
            let Some(rel) = path.strip_prefix(&prefix) else {
                continue;
            };
            if rel == dir || rel.starts_with(&format!("{dir}/")) {
                if rel
                    .split('/')
                    .skip(dir.split('/').count())
                    .any(|s| SKIP_DIRS.contains(&s) || s.starts_with('.') || s.ends_with(".bak"))
                {
                    continue;
                }
                all.push((rel.to_string(), size));
            }
        }
        let skill_md_path = format!("{prefix}{dir}/SKILL.md");
        let total_bytes: u64 = all.iter().map(|(_, s)| *s).sum();
        let files = all
            .into_iter()
            .map(|(rel, size)| DiscoveredFile {
                path: rel[dir.len() + 1..].to_string(),
                sha_path: rel,
                size,
            })
            .collect();
        out.push(DiscoveredSkill {
            name,
            dir_path: dir,
            skill_md_path,
            files,
            total_bytes,
        });
    }
    out
}

/// Static verdicts that need only paths/sizes (before fetching bytes).
fn pre_fetch_verdicts(skills: &[DiscoveredSkill]) -> (Vec<Refusal>, Vec<String>) {
    let mut refusals = Vec::new();
    let mut notes = Vec::new();
    for sk in skills {
        if control::is_bundled("skills", &sk.name) {
            refusals.push(Refusal {
                skill: sk.name.clone(),
                file: String::new(),
                rule: "bundled_collision".into(),
                detail: "name ships inside the RustFox binary — install would be undone \
                         by the next update; rename the skill in the source repo"
                    .into(),
            });
            continue;
        }
        if sk.files.len() > MAX_FILES_PER_SKILL {
            refusals.push(Refusal {
                skill: sk.name.clone(),
                file: String::new(),
                rule: "file_cap".into(),
                detail: format!("{} files > {MAX_FILES_PER_SKILL} cap", sk.files.len()),
            });
            continue;
        }
        if sk.total_bytes > MAX_SKILL_BYTES as u64 {
            refusals.push(Refusal {
                skill: sk.name.clone(),
                file: String::new(),
                rule: "size_cap".into(),
                detail: format!("{} bytes > {MAX_SKILL_BYTES} cap", sk.total_bytes),
            });
            continue;
        }
        for f in &sk.files {
            if has_executable_ext(&f.path) {
                refusals.push(Refusal {
                    skill: sk.name.clone(),
                    file: f.path.clone(),
                    rule: "executable_refused".into(),
                    detail: "executable-code extensions are never installed; this skill \
                             needs a manual, eyes-open install"
                        .into(),
                });
            } else if f.size > MAX_FILE_BYTES as u64 {
                refusals.push(Refusal {
                    skill: sk.name.clone(),
                    file: f.path.clone(),
                    rule: "file_size_cap".into(),
                    detail: format!("{} bytes > {MAX_FILE_BYTES}", f.size),
                });
            }
        }
    }
    if skills.len() > MAX_SKILLS_PER_REQUEST {
        notes.push(format!(
            "repo declares {} skills; this request installs the first {MAX_SKILLS_PER_REQUEST} \
             (narrow the source with :subpath)",
            skills.len()
        ));
    }
    (refusals, notes)
}

// ---------------------------------------------------------------------------
// API types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallRequest {
    pub source: String,
    #[serde(default)]
    pub dry_run: bool,
    /// Exact warning strings (as returned by the dry run) the owner accepts.
    #[serde(default)]
    pub acknowledged_warnings: Vec<String>,
    /// Quarantine an existing same-name dir before installing.
    #[serde(default)]
    pub force: bool,
}

fn warning_key(w: &Warning) -> String {
    format!("[{}] {}: {} — {}", w.rule, w.skill, w.file, w.detail)
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

pub async fn install_skill(
    State(state): State<PortalState>,
    Json(req): Json<InstallRequest>,
) -> Result<Json<Value>, PortalError> {
    let spec = parse_source(&req.source)?;
    let fetcher = state.fetcher.clone();

    // 1) resolve ref → sha
    let sha = resolve_sha(&spec, &fetcher).await?;

    // 2) recursive tree
    let tree_url = format!(
        "https://api.github.com/repos/{}/{}/git/trees/{}?recursive=1",
        spec.owner, spec.repo, sha
    );
    let tree = fetcher.get_json(&tree_url).await.map_err(map_fetch_err)?;
    let discovered = discover_skills(&spec, &tree);
    if discovered.is_empty() {
        return Err(PortalError::bad_request(
            "no_skills_found",
            "no SKILL.md within depth limits — try owner/repo:subpath",
        ));
    }
    let candidates: Vec<DiscoveredSkill> = discovered
        .into_iter()
        .take(MAX_SKILLS_PER_REQUEST)
        .collect();

    // 3) static verdicts
    let (mut refusals, notes) = pre_fetch_verdicts(&candidates);
    let refused_names: Vec<String> = refusals.iter().map(|r| r.skill.clone()).collect();

    // 4) fetch content for non-refused skills, run byte-level scanners
    let mut warnings: Vec<Warning> = Vec::new();
    let mut fetched: Vec<(DiscoveredSkill, Vec<(DiscoveredFile, String)>)> = Vec::new();
    let mut summaries: Vec<Value> = Vec::new();
    for sk in candidates
        .iter()
        .filter(|s| !refused_names.contains(&s.name))
    {
        let mut contents = Vec::new();
        let mut skill_aborted = false;
        for f in &sk.files {
            let url = format!(
                "https://raw.githubusercontent.com/{}/{}/{}/{}",
                spec.owner, spec.repo, sha, f.sha_path
            );
            let bytes = match fetcher.get_bytes(&url).await {
                Ok(b) => b,
                Err(e) if e.starts_with("github_rate_limited") => return Err(map_fetch_err(e)),
                Err(e) => {
                    refusals.push(Refusal {
                        skill: sk.name.clone(),
                        file: f.path.clone(),
                        rule: "fetch_failed".into(),
                        detail: e,
                    });
                    skill_aborted = true;
                    break;
                }
            };
            if bytes.len() > MAX_FILE_BYTES {
                refusals.push(Refusal {
                    skill: sk.name.clone(),
                    file: f.path.clone(),
                    rule: "file_size_cap".into(),
                    detail: format!("{} bytes > {MAX_FILE_BYTES}", bytes.len()),
                });
                skill_aborted = true;
                break;
            }
            let text = match String::from_utf8(bytes.clone()) {
                Ok(t) => t,
                Err(_) => {
                    refusals.push(Refusal {
                        skill: sk.name.clone(),
                        file: f.path.clone(),
                        rule: "binary_file".into(),
                        detail: "non-utf8 file — install refuses binaries".into(),
                    });
                    skill_aborted = true;
                    break;
                }
            };
            for (rule, detail) in secret_findings(&text) {
                refusals.push(Refusal {
                    skill: sk.name.clone(),
                    file: f.path.clone(),
                    rule: rule.into(),
                    detail,
                });
            }
            for (rule, detail) in injection_findings(&text) {
                warnings.push(Warning {
                    skill: sk.name.clone(),
                    file: f.path.clone(),
                    rule: rule.into(),
                    detail,
                });
            }
            contents.push((f.clone(), text));
        }
        if skill_aborted || refusals.iter().any(|r| r.skill == sk.name) {
            // a post-fetch refusal (secret/binary) knocks the skill out of
            // both the plan summary and the install set — the dry run must
            // show exactly what a real install would write
            continue;
        }
        let skill_md = contents
            .iter()
            .find(|(f, _)| f.path == "SKILL.md")
            .map(|(_, t)| t.clone())
            .unwrap_or_default();
        let description = frontmatter_description(&skill_md).unwrap_or_default();
        summaries.push(json!({
            "name": sk.name,
            "description": description,
            "files": sk.files.iter().map(|f| json!({"path": f.path, "size": f.size})).collect::<Vec<_>>(),
            "sizeBytes": sk.total_bytes,
        }));
        fetched.push((sk.clone(), contents));
    }
    let fetched: Vec<_> = fetched
        .into_iter()
        .filter(|(sk, _)| !refusals.iter().any(|r| r.skill == sk.name))
        .collect();
    let verdict = json!({
        "refused": refusals.iter().map(|r| serde_json::to_value(r).unwrap()).collect::<Vec<_>>(),
        "warnings": warnings.iter().map(|w| serde_json::to_value(w).unwrap()).collect::<Vec<_>>(),
        "notes": notes,
    });

    if req.dry_run {
        return Ok(Json(json!({
            "dryRun": true,
            "source": { "owner": spec.owner, "repo": spec.repo, "ref": sha },
            "skills": summaries,
            "verdict": verdict,
        })));
    }

    // 5) acknowledgement gate (ADR 0011a R2): every warning must be covered
    let unacked: Vec<&Warning> = warnings
        .iter()
        .filter(|w| !req.acknowledged_warnings.contains(&warning_key(w)))
        .collect();
    if !unacked.is_empty() {
        return Err(PortalError::new(
            axum::http::StatusCode::CONFLICT,
            "warnings_unacknowledged",
            format!(
                "{} warning(s) need explicit acknowledgement (re-run dryRun to read them, \
                 then send them back in acknowledgedWarnings): {}",
                unacked.len(),
                unacked
                    .iter()
                    .take(5)
                    .map(|w| warning_key(w))
                    .collect::<Vec<_>>()
                    .join(" | ")
            ),
        ));
    }

    // 6) write phase
    let home = control::home_dir(&state)?;
    let mut ledger = control::read_ledger(&home);
    let mut installed = Vec::new();
    let mut skipped = Vec::new();
    for (sk, contents) in &fetched {
        let dir = control::dir_for(&state, "skills", &sk.name);
        if dir.exists() {
            if !req.force {
                skipped.push(
                    json!({"name": sk.name, "reason": "exists (use force to quarantine+replace)"}),
                );
                continue;
            }
            let _ = control::quarantine(&state, "skills", &sk.name)
                .await
                .map_err(|e| {
                    PortalError::internal(format!(
                        "force-quarantine of {} failed: {}",
                        sk.name, e.message
                    ))
                })?;
        }
        for (f, text) in contents {
            let target = dir.join(&f.path);
            write_skill_file(&target, text).await?;
        }
        let content_hash = hash_skill_dir(&dir).unwrap_or_default();
        ledger.skills.insert(
            sk.name.clone(),
            InstalledRecord {
                source_repo: format!("{}/{}", spec.owner, spec.repo),
                commit_sha: sha.clone(),
                git_ref: spec.git_ref.clone().unwrap_or_default(),
                installed_at: chrono::Utc::now().to_rfc3339(),
                content_hash,
            },
        );
        installed.push(sk.name.clone());
    }
    control::write_ledger(&home, &ledger)?;
    let counts = state.agent.reload_skills_and_agents().await;
    tracing::info!(
        "Portal install: {} installed, {} skipped, {} refused, {} warnings (acked)",
        installed.len(),
        skipped.len(),
        refusals.len(),
        warnings.len()
    );
    Ok(Json(json!({
        "dryRun": false,
        "installed": installed,
        "skipped": skipped,
        "verdict": verdict,
        "reload": { "skillsLoaded": counts.0, "agentsLoaded": counts.1 },
    })))
}

async fn write_skill_file(target: &std::path::Path, content: &str) -> Result<(), PortalError> {
    if let Some(parent) = target.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(PortalError::internal)?;
    }
    // Fresh files only — no .bak noise for an installer (nothing was there
    // before). tmp+rename keeps a half-written dir impossible.
    let tmp = target.with_file_name(format!(
        "{}.tmp",
        target
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
    ));
    tokio::fs::write(&tmp, content)
        .await
        .map_err(PortalError::internal)?;
    tokio::fs::rename(&tmp, target)
        .await
        .map_err(PortalError::internal)?;
    Ok(())
}

fn map_fetch_err(e: String) -> PortalError {
    if e.starts_with("github_rate_limited") {
        PortalError::bad_request("github_rate_limited", e)
    } else {
        PortalError::bad_request("fetch_failed", e)
    }
}

async fn resolve_sha(
    spec: &SourceSpec,
    fetcher: &Arc<dyn GitHubFetcher>,
) -> Result<String, PortalError> {
    if let Some(r) = &spec.git_ref {
        if r.len() == 40 && r.chars().all(|c| c.is_ascii_hexdigit()) {
            return Ok(r.clone()); // already a sha
        }
        let url = format!(
            "https://api.github.com/repos/{}/{}/commits/{}",
            spec.owner, spec.repo, r
        );
        let v = fetcher.get_json(&url).await.map_err(map_fetch_err)?;
        return v
            .get("sha")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| PortalError::bad_request("fetch_failed", "commit lookup: no sha"));
    }
    let url = format!("https://api.github.com/repos/{}/{}", spec.owner, spec.repo);
    let v = fetcher.get_json(&url).await.map_err(map_fetch_err)?;
    let branch = v
        .get("default_branch")
        .and_then(Value::as_str)
        .unwrap_or("main");
    let url = format!(
        "https://api.github.com/repos/{}/{}/commits/{}",
        spec.owner, spec.repo, branch
    );
    let v = fetcher.get_json(&url).await.map_err(map_fetch_err)?;
    v.get("sha")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| PortalError::bad_request("fetch_failed", "default branch: no sha"))
}

pub fn frontmatter_description(content: &str) -> Option<String> {
    let rest = content.trim_start().strip_prefix("---")?;
    let end = rest.find("\n---")?;
    let block = &rest[..end];
    // single-line form
    if let Some(v) = block
        .lines()
        .find_map(|l| l.trim_end().strip_prefix("description:"))
    {
        let v = v.trim().trim_matches('"').trim_matches('\'');
        // bare |, >, |- etc. are block markers, not the value
        if !v.is_empty() && !v.chars().all(|c| matches!(c, '|' | '>' | '-' | '+')) {
            return Some(v.to_string());
        }
    }
    // block scalar (| or >)
    let mut acc = Vec::new();
    let mut capturing = false;
    for line in block.lines() {
        if capturing {
            if line.starts_with("  ") || line.trim().is_empty() {
                acc.push(line.trim());
            } else {
                break;
            }
        } else if line.trim_end().ends_with("description: |")
            || line.trim_end().ends_with("description: >")
        {
            capturing = true;
        }
    }
    if acc.is_empty() {
        None
    } else {
        Some(acc.join(" "))
    }
}

/// GET /api/skills/installed — provenance ledger view.
pub async fn installed_list(State(state): State<PortalState>) -> Result<Json<Value>, PortalError> {
    let home = control::home_dir(&state)?;
    let ledger = control::read_ledger(&home);
    Ok(Json(json!({
        "skills": ledger.skills,
        "agents": ledger.agents,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::embed::bundled_skill_names;

    fn file(path: &str, size: u64) -> DiscoveredFile {
        DiscoveredFile {
            path: path.into(),
            sha_path: format!("skills/x/{path}"),
            size,
        }
    }

    fn skill(name: &str, files: Vec<DiscoveredFile>) -> DiscoveredSkill {
        let total = files.iter().map(|f| f.size).sum();
        DiscoveredSkill {
            name: name.into(),
            dir_path: format!("skills/{name}"),
            skill_md_path: format!("skills/{name}/SKILL.md"),
            files,
            total_bytes: total,
        }
    }

    #[test]
    fn bundled_name_is_refused_before_anything_lands() {
        // A repo carrying a skill whose name collides with a real bundled
        // skill is a "lie" install (the update engine re-seeds) — refuse.
        let real = bundled_skill_names()
            .into_iter()
            .next()
            .expect("embedded skills exist");
        let sk = skill(&real, vec![file("SKILL.md", 50)]);
        let (refused, _notes) = pre_fetch_verdicts(std::slice::from_ref(&sk));
        assert!(
            refused
                .iter()
                .any(|r| r.rule == "bundled_collision" && r.skill == real),
            "bundled {real} must be refused: {refused:?}"
        );
    }

    #[test]
    fn caps_enforced() {
        // file cap
        let many = skill(
            "many",
            (0..=MAX_FILES_PER_SKILL)
                .map(|i| file(&format!("f{i}.md"), 10))
                .collect(),
        );
        let (refused, _) = pre_fetch_verdicts(&[many]);
        assert!(refused.iter().any(|r| r.rule == "file_cap"));
        // skill size cap
        let big = skill("big", vec![file("SKILL.md", MAX_SKILL_BYTES as u64 + 1)]);
        let (refused, _) = pre_fetch_verdicts(&[big]);
        assert!(refused.iter().any(|r| r.rule == "size_cap"));
        // single-file size cap
        let onefile = skill("one", vec![file("SKILL.md", MAX_FILE_BYTES as u64 + 1)]);
        let (refused, _) = pre_fetch_verdicts(&[onefile]);
        assert!(refused.iter().any(|r| r.rule == "file_size_cap"));
    }
}
