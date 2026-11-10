//! Official MCP Registry read proxy and install drafts (ADR-0041 §11).
//!
//! The portal pins `https://registry.modelcontextprotocol.io` `/v0.1` and
//! always asks for `version=latest`. Install does not write config here:
//! each draft tells the client how to call the existing Custom upsert
//! `PUT /api/settings/mcp/{slug}`.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::mcp_presets::{is_credential_header_name, validate_custom_name};

pub(crate) const DEFAULT_REGISTRY_BASE: &str = "https://registry.modelcontextprotocol.io";

const OFFICIAL_META: &str = "io.modelcontextprotocol.registry/official";
const SEARCH_MIN_CHARS: usize = 3;
const LIMIT_MAX: u32 = 100;
const CURSOR_MAX_BYTES: usize = 512;
const NAME_MAX_BYTES: usize = 512;
const SLUG_SUFFIX_MAX: u32 = 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExistingServer {
    pub name: String,
    pub url: Option<String>,
    /// Stdio rows keep `command`. Registry install must not reuse that slug.
    pub has_command: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RegistryError {
    SearchTooShort,
    InvalidLimit,
    InvalidCursor,
    InvalidName,
    NotFound,
    Unavailable,
}

#[derive(Debug)]
enum Upstream {
    Http(u16),
    Unreachable,
}

/// `None`, blank, or a non-http(s) value falls back to the official base.
pub(crate) fn normalize_registry_base(raw: Option<&str>) -> String {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return DEFAULT_REGISTRY_BASE.to_string();
    };
    let Ok(mut url) = reqwest::Url::parse(raw) else {
        return DEFAULT_REGISTRY_BASE.to_string();
    };
    if url.scheme() != "http" && url.scheme() != "https" {
        return DEFAULT_REGISTRY_BASE.to_string();
    }
    if url.set_username("").is_err() || url.set_password(None).is_err() {
        return DEFAULT_REGISTRY_BASE.to_string();
    }
    url.set_query(None);
    url.set_fragment(None);
    url.to_string().trim_end_matches('/').to_string()
}

pub(crate) fn registry_base_from_env() -> String {
    normalize_registry_base(std::env::var("RUSTFOX_MCP_REGISTRY_URL").ok().as_deref())
}

pub(crate) fn registry_http_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(10))
            .user_agent("rustfox-portal")
            .redirect(reqwest::redirect::Policy::limited(3))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

pub(crate) async fn proxy_search(
    client: &reqwest::Client,
    base: &str,
    q: &str,
    cursor: &str,
    limit: Option<u32>,
    existing: &[ExistingServer],
) -> Result<Value, RegistryError> {
    let url = build_list_url(base, q, cursor, limit)?;
    let mut body = match fetch_json(client, &url).await {
        Ok(body) => body,
        Err(Upstream::Http(422)) => return Err(RegistryError::SearchTooShort),
        Err(_) => return Err(RegistryError::Unavailable),
    };
    annotate_list(&mut body, existing);
    Ok(body)
}

pub(crate) async fn proxy_detail(
    client: &reqwest::Client,
    base: &str,
    name: &str,
    existing: &[ExistingServer],
) -> Result<Value, RegistryError> {
    let url = build_detail_url(base, name)?;
    let mut body = match fetch_json(client, &url).await {
        Ok(body) => body,
        Err(Upstream::Http(404)) => return Err(RegistryError::NotFound),
        Err(_) => return Err(RegistryError::Unavailable),
    };
    annotate_entry(&mut body, existing);
    Ok(body)
}

fn build_list_url(
    base: &str,
    q: &str,
    cursor: &str,
    limit: Option<u32>,
) -> Result<String, RegistryError> {
    if !q.is_empty() && q.trim().chars().count() < SEARCH_MIN_CHARS {
        return Err(RegistryError::SearchTooShort);
    }
    if let Some(limit) = limit {
        if !(1..=LIMIT_MAX).contains(&limit) {
            return Err(RegistryError::InvalidLimit);
        }
    }
    let cursor = cursor.trim();
    if cursor.len() > CURSOR_MAX_BYTES || cursor.chars().any(|c| c.is_control()) {
        return Err(RegistryError::InvalidCursor);
    }

    let mut url = reqwest::Url::parse(&format!("{}/v0.1/servers", base.trim_end_matches('/')))
        .map_err(|_| RegistryError::Unavailable)?;
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("version", "latest");
        if !q.is_empty() {
            // Length uses the trimmed query. Matching keeps the raw value,
            // including surrounding whitespace (registry rule).
            pairs.append_pair("search", q);
        }
        if let Some(limit) = limit {
            pairs.append_pair("limit", &limit.to_string());
        }
        if !cursor.is_empty() {
            pairs.append_pair("cursor", cursor);
        }
    }
    Ok(url.into())
}

fn build_detail_url(base: &str, name: &str) -> Result<String, RegistryError> {
    let name = validate_registry_name(name)?;
    let encoded = encode_path_segment(name);
    let url = format!(
        "{}/v0.1/servers/{encoded}/versions/latest",
        base.trim_end_matches('/')
    );
    reqwest::Url::parse(&url).map_err(|_| RegistryError::InvalidName)?;
    Ok(url)
}

fn validate_registry_name(name: &str) -> Result<&str, RegistryError> {
    let name = name.trim();
    if name.is_empty()
        || name.len() > NAME_MAX_BYTES
        || name.chars().any(|c| c.is_control() || c.is_whitespace())
        || name.contains(['?', '#', '\\', '%'])
        || name
            .split('/')
            .any(|seg| seg.is_empty() || seg == "." || seg == "..")
    {
        return Err(RegistryError::InvalidName);
    }
    Ok(name)
}

fn encode_path_segment(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

async fn fetch_json(client: &reqwest::Client, url: &str) -> Result<Value, Upstream> {
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|err| {
            let kind = if err.is_timeout() {
                "timeout"
            } else if err.is_connect() {
                "connect"
            } else {
                "request"
            };
            tracing::warn!("MCP registry unreachable ({kind})");
            Upstream::Unreachable
        })?;
    let status = response.status();
    if !status.is_success() {
        tracing::warn!(status = %status, "MCP registry returned an error");
        return Err(Upstream::Http(status.as_u16()));
    }
    response.json().await.map_err(|_| {
        tracing::warn!("MCP registry returned a non-JSON body");
        Upstream::Unreachable
    })
}

fn annotate_list(body: &mut Value, existing: &[ExistingServer]) {
    let Some(root) = body.as_object_mut() else {
        return;
    };
    // The registry schema allows `servers: null`. The portal UI reads
    // `servers.length` and `metadata.nextCursor`, so both keys are always set.
    let servers_missing = !matches!(root.get("servers"), Some(Value::Array(_)));
    if servers_missing {
        root.insert("servers".to_string(), Value::Array(Vec::new()));
    }
    if !root.contains_key("metadata") {
        root.insert(
            "metadata".to_string(),
            json!({ "count": 0, "nextCursor": Value::Null }),
        );
    }
    let Some(servers) = root.get_mut("servers").and_then(Value::as_array_mut) else {
        return;
    };
    for entry in servers {
        annotate_entry(entry, existing);
    }
}

fn annotate_entry(entry: &mut Value, existing: &[ExistingServer]) {
    if !entry.is_object() {
        return;
    }
    let install = install_draft(entry, existing);
    entry
        .as_object_mut()
        .expect("object checked")
        .insert("install".to_string(), install);
}

fn install_draft(entry: &Value, existing: &[ExistingServer]) -> Value {
    let server = entry.get("server").unwrap_or(entry);
    let registry_name = server
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let description = server.get("description").and_then(Value::as_str);
    let version = server.get("version").and_then(Value::as_str);
    let title = server.get("title").and_then(Value::as_str);
    let official = entry
        .get("_meta")
        .and_then(|meta| meta.get(OFFICIAL_META))
        .or_else(|| server.get("_meta").and_then(|meta| meta.get(OFFICIAL_META)));
    let status = official
        .and_then(|meta| meta.get("status"))
        .and_then(Value::as_str);
    let is_latest = official
        .and_then(|meta| meta.get("isLatest"))
        .and_then(Value::as_bool);

    let has_packages = server
        .get("packages")
        .and_then(Value::as_array)
        .is_some_and(|packages| !packages.is_empty());
    let remotes = server
        .get("remotes")
        .and_then(Value::as_array)
        .map(|items| items.as_slice())
        .unwrap_or(&[]);
    let remote_types = remote_type_list(remotes);
    let chosen = choose_remote(remotes, has_packages);

    let install_url = chosen.installable.then(|| chosen.url.clone()).flatten();
    let suggested = suggest_slug(&registry_name, existing, install_url.as_deref());

    let (bearer, headers) = if chosen.installable {
        map_headers(chosen.headers)
    } else {
        (None, Vec::new())
    };

    json!({
        "registryName": registry_name,
        "suggestedSlug": suggested,
        "installable": chosen.installable,
        "blockReason": chosen.block_reason,
        "transport": chosen.transport,
        "remoteTypes": remote_types,
        "hasPackages": has_packages,
        "url": if chosen.installable { chosen.url } else { chosen.display_url },
        "description": description,
        "version": version,
        "title": title,
        "status": status,
        "isLatest": is_latest,
        "enabled": true,
        "bearer": bearer,
        "headers": headers,
    })
}

struct ChosenRemote<'a> {
    installable: bool,
    block_reason: Option<&'static str>,
    transport: Option<&'static str>,
    url: Option<String>,
    display_url: Option<String>,
    headers: &'a [Value],
}

fn choose_remote(remotes: &[Value], has_packages: bool) -> ChosenRemote<'_> {
    if let Some(remote) = remotes
        .iter()
        .find(|remote| remote_type(remote) == "streamable-http")
    {
        let headers = remote_headers(remote);
        return match classify_url(remote.get("url").and_then(Value::as_str).unwrap_or("")) {
            UrlClass::Ok(url) => ChosenRemote {
                installable: true,
                block_reason: None,
                transport: Some("streamable-http"),
                url: Some(url),
                display_url: None,
                headers,
            },
            UrlClass::Rejected => ChosenRemote {
                installable: false,
                block_reason: Some("unsupported"),
                transport: Some("streamable-http"),
                url: None,
                display_url: None,
                headers,
            },
        };
    }

    if let Some(remote) = remotes.iter().find(|remote| remote_type(remote) == "sse") {
        let display = classify_url(remote.get("url").and_then(Value::as_str).unwrap_or(""));
        return ChosenRemote {
            installable: false,
            block_reason: Some("sse"),
            transport: Some("sse"),
            url: None,
            display_url: match display {
                UrlClass::Ok(url) => Some(url),
                UrlClass::Rejected => None,
            },
            headers: &[],
        };
    }

    ChosenRemote {
        installable: false,
        block_reason: Some(if has_packages {
            "stdio"
        } else if remotes.is_empty() {
            "no_remote"
        } else {
            "unsupported"
        }),
        transport: None,
        url: None,
        display_url: None,
        headers: &[],
    }
}

fn remote_type(remote: &Value) -> String {
    remote
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

fn remote_type_list(remotes: &[Value]) -> Vec<String> {
    let mut types = Vec::new();
    for remote in remotes {
        let kind = remote_type(remote);
        if !kind.is_empty() && !types.iter().any(|seen| seen == &kind) {
            types.push(kind);
        }
    }
    types
}

fn remote_headers(remote: &Value) -> &[Value] {
    remote
        .get("headers")
        .and_then(Value::as_array)
        .map(|items| items.as_slice())
        .unwrap_or(&[])
}

enum UrlClass {
    Ok(String),
    Rejected,
}

fn classify_url(raw: &str) -> UrlClass {
    let raw = raw.trim();
    if raw.is_empty() || raw.contains('{') || raw.contains('}') {
        return UrlClass::Rejected;
    }
    let Ok(url) = reqwest::Url::parse(raw) else {
        return UrlClass::Rejected;
    };
    if url.scheme() != "http" && url.scheme() != "https" {
        return UrlClass::Rejected;
    }
    if !url.username().is_empty() || url.password().is_some() {
        return UrlClass::Rejected;
    }
    if crate::mcp_presets::url_may_contain_credential(raw) {
        return UrlClass::Rejected;
    }
    UrlClass::Ok(raw.to_string())
}

fn map_headers(headers: &[Value]) -> (Option<Value>, Vec<Value>) {
    let mut bearer = None;
    let mut out = Vec::new();
    for header in headers {
        let Some(name) = header.get("name").and_then(Value::as_str).map(str::trim) else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        let value = header
            .get("value")
            .and_then(Value::as_str)
            .or_else(|| header.get("default").and_then(Value::as_str))
            .unwrap_or("")
            .trim();
        let placeholders = placeholders(value);
        let secret = header
            .get("isSecret")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || is_credential_header_name(name);
        let required = header
            .get("isRequired")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || secret
            || !placeholders.is_empty();
        if bearer.is_none() && maps_to_bearer(name, value, header) {
            bearer = Some(json!({
                "required": required,
                "placeholders": placeholders,
            }));
            continue;
        }
        let literal = if secret || !placeholders.is_empty() || value.is_empty() {
            None
        } else {
            Some(value.to_string())
        };
        out.push(json!({
            "name": name,
            "isSecret": secret,
            "required": required,
            "mapsTo": "header",
            "placeholders": placeholders,
            "literal": literal,
        }));
    }
    (bearer, out)
}

fn maps_to_bearer(name: &str, value: &str, header: &Value) -> bool {
    if !name.eq_ignore_ascii_case("authorization") {
        return false;
    }
    let lower = value.to_ascii_lowercase();
    if lower.is_empty() || lower == "bearer" || lower.starts_with("bearer ") {
        return true;
    }
    // `Basic …` and other HTTP auth schemes stay on the header map.
    if let Some((scheme, _)) = lower.split_once(' ') {
        if scheme != "bearer" && scheme.chars().all(|c| c.is_ascii_alphabetic()) {
            return false;
        }
    }
    header
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase()
        .contains("bearer")
        || !placeholders(value).is_empty()
}

fn placeholders(value: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = value;
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            break;
        };
        let name = &after[..end];
        if !name.is_empty() && !name.contains('{') && !out.iter().any(|seen| seen == name) {
            out.push(name.to_string());
        }
        rest = &after[end + 1..];
    }
    out
}

fn suggest_slug(
    registry_name: &str,
    existing: &[ExistingServer],
    install_url: Option<&str>,
) -> String {
    if let Some(url) = install_url {
        if let Some(reused) = existing.iter().find(|server| {
            !server.has_command
                && server.url.as_deref().map(str::trim) == Some(url)
                && validate_custom_name(&server.name).is_ok()
        }) {
            return reused.name.clone();
        }
    }

    let hash = short_hash(registry_name);
    let base = slug_base(registry_name);
    let base = if base.is_empty() {
        format!("mcp-{hash}")
    } else {
        base
    };
    if !slug_taken(&base, existing) {
        return base;
    }
    for n in 2..=SLUG_SUFFIX_MAX {
        let candidate = format!("{base}-{n}");
        if !slug_taken(&candidate, existing) {
            return candidate;
        }
    }
    let hashed = format!("{base}-{hash}");
    if !slug_taken(&hashed, existing) {
        return hashed;
    }
    for n in 2..=SLUG_SUFFIX_MAX {
        let candidate = format!("{hashed}-{n}");
        if !slug_taken(&candidate, existing) {
            return candidate;
        }
    }
    format!(
        "{hashed}-{}",
        short_hash(&format!("{registry_name}:{hashed}"))
    )
}

fn slug_base(registry_name: &str) -> String {
    let segment = registry_name.trim().rsplit('/').next().unwrap_or("").trim();
    let mut out = String::new();
    for c in segment.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-' {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

fn slug_taken(name: &str, existing: &[ExistingServer]) -> bool {
    validate_custom_name(name).is_err()
        || existing
            .iter()
            .any(|server| server.name.eq_ignore_ascii_case(name))
}

fn short_hash(input: &str) -> String {
    let digest = Sha256::digest(input.as_bytes());
    format!("{:02x}{:02x}{:02x}", digest[0], digest[1], digest[2])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn list_url(q: &str, cursor: &str, limit: Option<u32>) -> Result<String, RegistryError> {
        build_list_url("https://registry.example", q, cursor, limit)
    }

    fn query_value(url: &str, key: &str) -> Option<String> {
        reqwest::Url::parse(url).ok().and_then(|url| {
            url.query_pairs()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.into_owned())
        })
    }

    fn entry(name: &str, remotes: Value, packages: Value) -> Value {
        json!({
            "server": {
                "name": name,
                "description": "Weather tools",
                "version": "1.2.3",
                "title": "Weather",
                "remotes": remotes,
                "packages": packages
            },
            "_meta": {
                "io.modelcontextprotocol.registry/official": {
                    "status": "active",
                    "isLatest": true
                }
            }
        })
    }

    fn http_remote(url: &str, headers: Value) -> Value {
        json!({
            "type": "streamable-http",
            "url": url,
            "headers": headers
        })
    }

    #[test]
    fn normalizes_registry_base_and_rejects_non_http() {
        assert_eq!(
            normalize_registry_base(None),
            "https://registry.modelcontextprotocol.io"
        );
        assert_eq!(
            normalize_registry_base(Some("https://registry.example/root/")),
            "https://registry.example/root"
        );
        assert_eq!(
            normalize_registry_base(Some("file:///tmp/registry")),
            DEFAULT_REGISTRY_BASE
        );
        assert_eq!(
            normalize_registry_base(Some("https://user:secret@registry.example/v0")),
            "https://registry.example/v0"
        );
    }

    #[test]
    fn search_query_gate_matches_registry_rules() {
        assert_eq!(list_url("", "", None).map(|_| ()), Ok(()));
        assert!(query_value(&list_url("", "", None).unwrap(), "search").is_none());
        assert_eq!(
            query_value(&list_url("", "", None).unwrap(), "version").as_deref(),
            Some("latest")
        );
        assert_eq!(list_url("ab", "", None), Err(RegistryError::SearchTooShort));
        assert_eq!(
            list_url("  x ", "", None),
            Err(RegistryError::SearchTooShort)
        );
        assert_eq!(
            list_url("   ", "", None),
            Err(RegistryError::SearchTooShort)
        );
        assert_eq!(
            list_url("検索", "", None),
            Err(RegistryError::SearchTooShort)
        );

        let forwarded = list_url("  abc  ", "", Some(50)).unwrap();
        assert_eq!(
            query_value(&forwarded, "search").as_deref(),
            Some("  abc  ")
        );
        assert_eq!(query_value(&forwarded, "limit").as_deref(), Some("50"));
        assert_eq!(
            query_value(&forwarded, "version").as_deref(),
            Some("latest")
        );
        let unicode = list_url("検索語", "cursor-1", None).unwrap();
        assert_eq!(query_value(&unicode, "search").as_deref(), Some("検索語"));
        assert_eq!(query_value(&unicode, "cursor").as_deref(), Some("cursor-1"));
    }

    #[test]
    fn limit_and_cursor_are_bounded() {
        assert_eq!(list_url("", "", Some(0)), Err(RegistryError::InvalidLimit));
        assert_eq!(
            list_url("", "", Some(101)),
            Err(RegistryError::InvalidLimit)
        );
        assert_eq!(
            list_url("", &"x".repeat(513), None),
            Err(RegistryError::InvalidCursor)
        );
        assert_eq!(
            list_url("", "bad\ncursor", None),
            Err(RegistryError::InvalidCursor)
        );
    }

    #[test]
    fn null_server_list_is_an_empty_array_with_metadata() {
        let mut body = json!({ "servers": null });
        annotate_list(&mut body, &[]);
        assert_eq!(body["servers"], json!([]));
        assert!(body["metadata"]["nextCursor"].is_null());
    }

    #[test]
    fn detail_url_encodes_the_registry_name() {
        let url = build_detail_url("https://registry.example/", "io.github.user/weather").unwrap();
        assert_eq!(
            url,
            "https://registry.example/v0.1/servers/io.github.user%2Fweather/versions/latest"
        );
        assert_eq!(
            build_detail_url("https://registry.example", "io.github.user/../weather"),
            Err(RegistryError::InvalidName)
        );
        assert_eq!(
            build_detail_url("https://registry.example", ""),
            Err(RegistryError::InvalidName)
        );
    }

    #[test]
    fn slug_uses_last_segment_and_replaces_invalid_characters() {
        let slug = suggest_slug("io.github.User/My Server!", &[], None);
        assert_eq!(slug, "my-server");
        assert_eq!(
            suggest_slug("com.example/foo.bar_v2", &[], None),
            "foo.bar_v2"
        );
        let empty = suggest_slug("io.github.user/", &[], None);
        assert!(empty.starts_with("mcp-"));
        assert!(validate_custom_name(&empty).is_ok());
    }

    #[test]
    fn slug_collision_appends_numeric_suffix_then_hash() {
        let existing = vec![ExistingServer {
            name: "weather".into(),
            url: None,
            has_command: false,
        }];
        assert_eq!(
            suggest_slug("io.github.acme/weather", &existing, None),
            "weather-2"
        );
        assert_eq!(suggest_slug("io.github.acme/exa", &[], None), "exa-2");
        assert_eq!(suggest_slug("io.github.acme/Google", &[], None), "google-2");

        let mut crowded = vec![ExistingServer {
            name: "Weather".into(),
            url: None,
            has_command: false,
        }];
        for n in 2..=20 {
            crowded.push(ExistingServer {
                name: format!("weather-{n}"),
                url: None,
                has_command: false,
            });
        }
        let hashed = suggest_slug("io.github.acme/weather", &crowded, None);
        assert_eq!(
            hashed,
            format!("weather-{}", short_hash("io.github.acme/weather"))
        );
        assert!(validate_custom_name(&hashed).is_ok());
    }

    #[test]
    fn same_remote_url_reuses_custom_slug_and_skips_stdio() {
        let existing = vec![
            ExistingServer {
                name: "already".into(),
                url: Some("https://example.com/mcp".into()),
                has_command: false,
            },
            ExistingServer {
                name: "weather".into(),
                url: Some("https://example.com/mcp".into()),
                has_command: true,
            },
        ];
        assert_eq!(
            suggest_slug(
                "io.github.acme/weather",
                &existing,
                Some("https://example.com/mcp")
            ),
            "already"
        );
    }

    #[test]
    fn install_draft_prefers_streamable_http_and_hides_secrets() {
        let secret = "super-secret-registry-token";
        let row = entry(
            "io.github.acme/weather",
            json!([
                {"type": "sse", "url": "https://example.com/sse"},
                http_remote(
                    "https://example.com/mcp",
                    json!([
                        {
                            "name": "Authorization",
                            "value": "Bearer {token}",
                            "isRequired": true,
                            "isSecret": true
                        },
                        {
                            "name": "X-Api-Key",
                            "value": secret,
                            "isSecret": true,
                            "isRequired": true
                        },
                        {"name": "X-Region", "value": "us", "isSecret": false}
                    ])
                )
            ]),
            json!([]),
        );
        let draft = install_draft(&row, &[]);
        assert_eq!(draft["installable"], true);
        assert!(draft["blockReason"].is_null());
        assert_eq!(draft["transport"], "streamable-http");
        assert_eq!(draft["url"], "https://example.com/mcp");
        assert_eq!(draft["suggestedSlug"], "weather");
        assert_eq!(draft["registryName"], "io.github.acme/weather");
        assert_eq!(draft["status"], "active");
        assert_eq!(draft["bearer"]["required"], true);
        assert_eq!(draft["bearer"]["placeholders"][0], "token");
        let headers = draft["headers"].as_array().unwrap();
        assert_eq!(headers.len(), 2);
        assert_eq!(headers[0]["name"], "X-Api-Key");
        assert!(headers[0]["literal"].is_null());
        assert_eq!(headers[0]["isSecret"], true);
        assert_eq!(headers[1]["literal"], "us");
        let rendered = draft.to_string();
        assert!(!rendered.contains(secret));
        assert!(!rendered.contains("Bearer {token}"));
    }

    #[test]
    fn sse_only_and_stdio_are_not_installable() {
        let sse = entry(
            "io.github.acme/events",
            json!([{"type": "sse", "url": "https://example.com/sse"}]),
            json!([]),
        );
        let sse_draft = install_draft(&sse, &[]);
        assert_eq!(sse_draft["installable"], false);
        assert_eq!(sse_draft["blockReason"], "sse");
        assert_eq!(sse_draft["url"], "https://example.com/sse");

        let stdio = entry(
            "io.github.acme/local",
            json!([]),
            json!([{"registryType": "npm", "identifier": "srv"}]),
        );
        let stdio_draft = install_draft(&stdio, &[]);
        assert_eq!(stdio_draft["installable"], false);
        assert_eq!(stdio_draft["blockReason"], "stdio");
        assert_eq!(stdio_draft["hasPackages"], true);

        let none = entry("io.github.acme/empty", json!([]), json!([]));
        assert_eq!(install_draft(&none, &[])["blockReason"], "no_remote");
    }

    #[test]
    fn templated_or_credential_urls_are_not_installable() {
        let templated = entry(
            "io.github.acme/tmpl",
            json!([http_remote("https://example.com/{host}/mcp", json!([]))]),
            json!([]),
        );
        assert_eq!(install_draft(&templated, &[])["blockReason"], "unsupported");
        let credential = entry(
            "io.github.acme/creds",
            json!([http_remote("https://user:pass@example.com/mcp", json!([]))]),
            json!([]),
        );
        let draft = install_draft(&credential, &[]);
        assert_eq!(draft["installable"], false);
        assert_eq!(draft["blockReason"], "unsupported");
        assert!(!draft.to_string().contains("user:pass"));
    }

    #[test]
    fn basic_authorization_stays_a_header() {
        let row = entry(
            "io.github.acme/basic",
            json!([http_remote(
                "https://example.com/mcp",
                json!([{
                    "name": "Authorization",
                    "value": "Basic {user}",
                    "isSecret": true
                }])
            )]),
            json!([]),
        );
        let draft = install_draft(&row, &[]);
        assert!(draft["bearer"].is_null());
        assert_eq!(draft["headers"][0]["name"], "Authorization");
        assert!(draft["headers"][0]["literal"].is_null());
        assert_eq!(draft["headers"][0]["placeholders"][0], "user");
    }

    #[tokio::test]
    async fn proxy_search_annotates_upstream_json_and_soft_fails() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/v0.1/servers"))
            .and(wiremock::matchers::query_param("version", "latest"))
            .and(wiremock::matchers::query_param("search", "weather"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(json!({
                "servers": [entry(
                    "io.github.acme/weather",
                    json!([http_remote("https://example.com/mcp", json!([]))]),
                    json!([])
                )],
                "metadata": { "count": 1, "nextCursor": "next-1" }
            })))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let body = proxy_search(&client, &server.uri(), "weather", "", Some(10), &[])
            .await
            .unwrap();
        assert_eq!(body["metadata"]["nextCursor"], "next-1");
        assert_eq!(body["servers"][0]["install"]["suggestedSlug"], "weather");
        assert_eq!(body["servers"][0]["install"]["installable"], true);
        assert_eq!(
            body["servers"][0]["server"]["name"],
            "io.github.acme/weather"
        );

        let short = proxy_search(&client, &server.uri(), "no", "", None, &[])
            .await
            .unwrap_err();
        assert_eq!(short, RegistryError::SearchTooShort);

        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/v0.1/servers"))
            .and(wiremock::matchers::query_param("version", "latest"))
            .respond_with(wiremock::ResponseTemplate::new(503))
            .mount(&server)
            .await;
        let down = proxy_search(&client, &server.uri(), "", "", None, &[])
            .await
            .unwrap_err();
        assert_eq!(down, RegistryError::Unavailable);
    }

    #[tokio::test]
    async fn proxy_detail_encodes_name_and_maps_404() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(
                "/v0.1/servers/io.github.acme%2Fweather/versions/latest",
            ))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(entry(
                "io.github.acme/weather",
                json!([http_remote("https://example.com/mcp", json!([]))]),
                json!([]),
            )))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let body = proxy_detail(&client, &server.uri(), "io.github.acme/weather", &[])
            .await
            .unwrap();
        assert_eq!(body["install"]["registryName"], "io.github.acme/weather");

        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(
                "/v0.1/servers/io.github.acme%2Fmissing/versions/latest",
            ))
            .respond_with(wiremock::ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let missing = proxy_detail(&client, &server.uri(), "io.github.acme/missing", &[])
            .await
            .unwrap_err();
        assert_eq!(missing, RegistryError::NotFound);
    }

    #[tokio::test]
    async fn unreachable_registry_is_unavailable() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(2))
            .build()
            .unwrap();
        let err = proxy_search(
            &client,
            &format!("http://127.0.0.1:{port}"),
            "",
            "",
            None,
            &[],
        )
        .await
        .unwrap_err();
        assert_eq!(err, RegistryError::Unavailable);
    }
}
