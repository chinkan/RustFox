# RustFox Web UI — Chat + Config + Google OAuth Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add a browser-based chat UI, a config editor page (replacing the setup binary), and an in-browser Google Workspace OAuth Device Code flow — all served from the running bot's Axum web server.

**Architecture:** `main.rs` detects setup-only mode (no valid config.toml) vs normal mode and starts the appropriate subset of services. The web server is an Axum router built by `src/web/mod.rs`; it holds `WebState { agent: Option<Arc<Agent>>, config_path }`. Chat streaming uses a `tokio::sync::broadcast` channel (`web_tx`) added to `Agent`. Config loading and formatting logic migrates from `src/bin/setup.rs` to `src/web/config_page.rs`; the setup binary is deleted. Templates are Askama 0.12 files in `templates/` at the project root.

**Tech Stack:** `axum 0.8`, `askama 0.12`, `tokio-stream 0.1`, `tower-http 0.6`, `reqwest 0.12` (already present), HTMX 2.x + Alpine.js 3.x (CDN)

**Already done (Task 1):** `Cargo.toml` has `askama`, `tower-http`, `tokio-stream`. `src/config.rs` has `WebConfig`. Committed as `feat(web): add WebConfig and web UI dependencies`.

---

## Task 2: Askama templates

**Files:**
- Create: `templates/base.html`
- Create: `templates/chat.html`
- Create: `templates/config.html`

Askama 0.12 resolves templates relative to the crate root `templates/` directory by default. No `askama.toml` needed.

**Step 1: Create `templates/base.html`**

```html
<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>RustFox — {% block title %}{% endblock %}</title>
  <script src="https://unpkg.com/htmx.org@2.0.4/dist/htmx.min.js" defer></script>
  <script src="https://unpkg.com/alpinejs@3.14.3/dist/cdn.min.js" defer></script>
  <style>
    *{box-sizing:border-box;margin:0;padding:0}
    body{font-family:system-ui,sans-serif;background:#0d1117;color:#e6edf3;min-height:100vh;display:flex;flex-direction:column}
    nav{padding:.75rem 1rem;background:#161b22;border-bottom:1px solid #30363d;display:flex;gap:1rem;align-items:center}
    nav a{color:#58a6ff;text-decoration:none;font-size:.9rem}
    nav a:hover{text-decoration:underline}
    .brand{font-weight:600;color:#e6edf3;margin-right:auto}
    main{flex:1;padding:1rem;max-width:900px;width:100%;margin:0 auto;display:flex;flex-direction:column}
    {% block extra_style %}{% endblock %}
  </style>
</head>
<body>
<nav>
  <span class="brand">RustFox</span>
  <a href="/">Chat</a>
  <a href="/config">Config</a>
</nav>
<main>
  {% block content %}{% endblock %}
</main>
</body>
</html>
```

**Step 2: Create `templates/chat.html`**

```html
{% extends "base.html" %}
{% block title %}Chat{% endblock %}
{% block extra_style %}
#messages{flex:1;overflow-y:auto;display:flex;flex-direction:column;gap:.75rem;padding-bottom:.5rem;min-height:200px}
.msg{padding:.6rem .9rem;border-radius:8px;max-width:80%;white-space:pre-wrap;word-break:break-word;line-height:1.5}
.msg.user{background:#1f6feb;align-self:flex-end}
.msg.assistant{background:#21262d;align-self:flex-start;border:1px solid #30363d}
.msg.assistant.streaming{border-color:#58a6ff}
.msg.error{background:#3d1c1c;border:1px solid #f85149;align-self:flex-start}
#input-bar{display:flex;gap:.5rem;padding-top:.75rem;border-top:1px solid #30363d;margin-top:.5rem}
#input-bar textarea{flex:1;background:#161b22;border:1px solid #30363d;color:#e6edf3;border-radius:6px;padding:.5rem .75rem;resize:none;font-size:.95rem;font-family:inherit}
#input-bar textarea:focus{outline:none;border-color:#58a6ff}
#input-bar button{background:#238636;color:#fff;border:none;border-radius:6px;padding:.5rem 1.2rem;cursor:pointer;font-size:.95rem}
#input-bar button:disabled{opacity:.5;cursor:not-allowed}
{% endblock %}
{% block content %}
<div id="messages" x-data x-ref="msgbox"></div>
<div id="input-bar" x-data="{
  text: '',
  busy: false,
  async send() {
    const msg = this.text.trim();
    if (!msg || this.busy) return;
    this.busy = true; this.text = '';
    const box = document.getElementById('messages');
    const u = document.createElement('div');
    u.className = 'msg user'; u.textContent = msg;
    box.appendChild(u); box.scrollTop = box.scrollHeight;
    const res = await fetch('/chat/send', {method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({text:msg})});
    const {session_id, error} = await res.json();
    if (error) {
      const e = document.createElement('div');
      e.className = 'msg error'; e.textContent = 'Error: ' + error;
      box.appendChild(e); this.busy = false; return;
    }
    const a = document.createElement('div');
    a.className = 'msg assistant streaming'; a.id = 'stream-' + session_id;
    box.appendChild(a); box.scrollTop = box.scrollHeight;
    const src = new EventSource('/chat/stream/' + session_id);
    src.addEventListener('token', e => { a.textContent += e.data; box.scrollTop = box.scrollHeight; });
    src.addEventListener('done', () => { a.classList.remove('streaming'); src.close(); this.busy = false; });
    src.onerror = () => { a.classList.remove('streaming'); a.classList.add('error'); src.close(); this.busy = false; };
  }
}">
  <textarea x-model="text" placeholder="Message RustFox…" rows="2" :disabled="busy"
    @keydown.enter.prevent="if (!$event.shiftKey) send()"></textarea>
  <button @click="send()" :disabled="busy || !text.trim()">Send</button>
</div>
{% endblock %}
```

**Step 3: Create `templates/config.html`**

This is the largest template. It renders the config form and the Google OAuth panel.

```html
{% extends "base.html" %}
{% block title %}Config{% endblock %}
{% block extra_style %}
.section{background:#161b22;border:1px solid #30363d;border-radius:8px;padding:1.25rem;margin-bottom:1rem}
.section h2{font-size:1rem;font-weight:600;margin-bottom:1rem;color:#58a6ff}
.field{margin-bottom:.75rem}
label{display:block;font-size:.85rem;color:#8b949e;margin-bottom:.3rem}
input,select,textarea{width:100%;background:#0d1117;border:1px solid #30363d;color:#e6edf3;border-radius:6px;padding:.4rem .6rem;font-size:.9rem;font-family:inherit}
input:focus,select:focus,textarea:focus{outline:none;border-color:#58a6ff}
textarea{resize:vertical;min-height:80px}
.btn{padding:.4rem .9rem;border:none;border-radius:6px;cursor:pointer;font-size:.85rem}
.btn-primary{background:#238636;color:#fff}
.btn-danger{background:#b91c1c;color:#fff}
.btn-secondary{background:#30363d;color:#e6edf3}
.btn:disabled{opacity:.5;cursor:not-allowed}
.mcp-server{background:#0d1117;border:1px solid #30363d;border-radius:6px;padding:1rem;margin-bottom:.75rem}
.env-row{display:grid;grid-template-columns:1fr 1fr auto;gap:.4rem;margin-bottom:.3rem}
.banner{padding:.6rem 1rem;border-radius:6px;margin-bottom:1rem;font-size:.9rem}
.banner.success{background:#0f2d17;border:1px solid #238636;color:#3fb950}
.banner.error{background:#3d1c1c;border:1px solid #f85149;color:#f85149}
.oauth-panel{background:#0d1117;border:1px solid #30363d;border-radius:6px;padding:1rem;margin-top:.75rem}
.code-box{font-family:monospace;font-size:1.3rem;letter-spacing:.15em;background:#161b22;border:1px solid #58a6ff;border-radius:6px;padding:.6rem 1rem;display:inline-block;color:#58a6ff}
{% endblock %}
{% block content %}
<div x-data="configApp()" x-init="loadConfig()">

  <template x-if="banner.text">
    <div class="banner" :class="banner.type" x-text="banner.text"></div>
  </template>

  <!-- Telegram -->
  <div class="section">
    <h2>Telegram</h2>
    <div class="field"><label>Bot Token</label><input type="password" x-model="cfg.telegram_token" placeholder="1234567890:ABC..."></div>
    <div class="field"><label>Allowed User IDs (comma-separated)</label><input x-model="cfg.allowed_user_ids" placeholder="123456789, 987654321"></div>
  </div>

  <!-- OpenRouter -->
  <div class="section">
    <h2>OpenRouter</h2>
    <div class="field"><label>API Key</label><input type="password" x-model="cfg.openrouter_key" placeholder="sk-or-..."></div>
    <div class="field"><label>Model</label><input x-model="cfg.model" placeholder="moonshotai/kimi-k2.5"></div>
    <div class="field"><label>Max Tokens</label><input type="number" x-model.number="cfg.max_tokens" placeholder="4096"></div>
    <div class="field"><label>System Prompt</label><textarea x-model="cfg.system_prompt" rows="4"></textarea></div>
  </div>

  <!-- Sandbox -->
  <div class="section">
    <h2>Sandbox</h2>
    <div class="field"><label>Allowed Directory</label><input x-model="cfg.sandbox_dir" placeholder="/tmp/rustfox-sandbox"></div>
  </div>

  <!-- Memory -->
  <div class="section">
    <h2>Memory</h2>
    <div class="field"><label>Database Path</label><input x-model="cfg.db_path" placeholder="rustfox.db"></div>
  </div>

  <!-- General -->
  <div class="section">
    <h2>General</h2>
    <div class="field"><label>Location (optional)</label><input x-model="cfg.location" placeholder="Tokyo, Japan"></div>
  </div>

  <!-- MCP Servers -->
  <div class="section">
    <h2>MCP Servers</h2>
    <template x-for="(srv, si) in cfg.mcp_servers" :key="si">
      <div class="mcp-server">
        <div style="display:flex;justify-content:space-between;margin-bottom:.5rem">
          <strong x-text="srv.name || '(unnamed)'"></strong>
          <button class="btn btn-danger" @click="cfg.mcp_servers.splice(si,1)">Remove</button>
        </div>
        <div class="field"><label>Name</label><input x-model="srv.name"></div>
        <div class="field"><label>Command</label><input x-model="srv.command" placeholder="uvx"></div>
        <div class="field"><label>Args (space-separated)</label><input :value="srv.args.join(' ')" @input="srv.args = $event.target.value.split(' ').filter(Boolean)"></div>
        <div class="field">
          <label>Environment Variables</label>
          <template x-for="(val, key, ei) in srv.env" :key="key">
            <div class="env-row">
              <input :value="key" @change="renameEnvKey(srv, key, $event.target.value)" placeholder="KEY">
              <input x-model="srv.env[key]" placeholder="value">
              <button class="btn btn-danger" @click="delete srv.env[key]">×</button>
            </div>
          </template>
          <button class="btn btn-secondary" style="margin-top:.3rem" @click="addEnvKey(srv)">+ Add env var</button>
        </div>
      </div>
    </template>
    <button class="btn btn-secondary" @click="addMcpServer()">+ Add MCP Server</button>

    <!-- Google Workspace quick-connect -->
    <div class="oauth-panel" x-show="showOAuth" x-cloak>
      <h3 style="font-size:.9rem;margin-bottom:.75rem;color:#58a6ff">Connect Google Workspace</h3>
      <p style="font-size:.82rem;color:#8b949e;margin-bottom:.75rem">
        Requires a Google Cloud OAuth Client ID (Desktop app type).
        <a href="https://console.cloud.google.com/apis/credentials" target="_blank" style="color:#58a6ff">Create one here</a>.
      </p>
      <div class="field"><label>Client ID</label><input x-model="oauth.clientId" placeholder="xxx.apps.googleusercontent.com"></div>
      <div class="field"><label>Client Secret</label><input type="password" x-model="oauth.clientSecret"></div>
      <button class="btn btn-primary" @click="startGoogleAuth()" :disabled="oauth.busy || !oauth.clientId || !oauth.clientSecret">
        <span x-show="!oauth.busy">Connect Google</span>
        <span x-show="oauth.busy">Waiting…</span>
      </button>
      <template x-if="oauth.userCode">
        <div style="margin-top:1rem">
          <p style="font-size:.85rem;margin-bottom:.5rem">1. Open this URL: <a :href="oauth.verificationUrl" target="_blank" style="color:#58a6ff" x-text="oauth.verificationUrl"></a></p>
          <p style="font-size:.85rem;margin-bottom:.5rem">2. Enter this code:</p>
          <div class="code-box" x-text="oauth.userCode"></div>
        </div>
      </template>
      <template x-if="oauth.error">
        <p style="color:#f85149;margin-top:.5rem;font-size:.85rem" x-text="oauth.error"></p>
      </template>
    </div>
    <button class="btn btn-secondary" style="margin-top:.5rem" @click="showOAuth = !showOAuth" x-text="showOAuth ? 'Hide Google Auth' : 'Connect Google Workspace'"></button>
  </div>

  <!-- Save -->
  <div style="margin-top:1rem">
    <button class="btn btn-primary" @click="saveConfig()" :disabled="saving" style="font-size:1rem;padding:.6rem 1.5rem">
      <span x-show="!saving">Save Config</span>
      <span x-show="saving">Saving…</span>
    </button>
  </div>

</div>

<script>
function configApp() {
  return {
    cfg: { telegram_token:'', allowed_user_ids:'', openrouter_key:'', model:'', max_tokens:4096, system_prompt:'', location:'', sandbox_dir:'', db_path:'', mcp_servers:[] },
    banner: { text:'', type:'success' },
    saving: false,
    showOAuth: false,
    oauth: { clientId:'', clientSecret:'', busy:false, userCode:'', verificationUrl:'', deviceCode:'', interval:5, error:'' },

    async loadConfig() {
      const r = await fetch('/api/load-config');
      const d = await r.json();
      if (d.exists) { Object.assign(this.cfg, d); }
    },

    addMcpServer() { this.cfg.mcp_servers.push({ name:'', command:'', args:[], env:{} }); },

    addEnvKey(srv) {
      let k = 'KEY'; let i = 0;
      while (k in srv.env) { k = 'KEY' + (++i); }
      srv.env[k] = '';
    },

    renameEnvKey(srv, oldKey, newKey) {
      if (oldKey === newKey) return;
      const val = srv.env[oldKey];
      delete srv.env[oldKey];
      srv.env[newKey] = val;
    },

    async saveConfig() {
      this.saving = true; this.banner = { text:'', type:'success' };
      const r = await fetch('/api/save-config', { method:'POST', headers:{'Content-Type':'application/json'}, body: JSON.stringify(this.cfg) });
      const d = await r.json();
      this.saving = false;
      if (d.ok) { this.banner = { text:'Saved. Restart the bot to apply changes.', type:'success' }; }
      else { this.banner = { text:'Save failed: ' + (d.error || 'unknown error'), type:'error' }; }
    },

    async startGoogleAuth() {
      this.oauth.busy = true; this.oauth.userCode = ''; this.oauth.error = '';
      const r = await fetch('/api/google-auth/start', { method:'POST', headers:{'Content-Type':'application/json'},
        body: JSON.stringify({ client_id: this.oauth.clientId, client_secret: this.oauth.clientSecret }) });
      const d = await r.json();
      if (d.error) { this.oauth.error = d.error; this.oauth.busy = false; return; }
      this.oauth.userCode = d.user_code;
      this.oauth.verificationUrl = d.verification_url;
      this.oauth.deviceCode = d.device_code;
      this.oauth.interval = d.interval;
      // Connect SSE poller
      const params = new URLSearchParams({ client_id: this.oauth.clientId, client_secret: this.oauth.clientSecret, interval: d.interval });
      const src = new EventSource('/api/google-auth/poll/' + encodeURIComponent(d.device_code) + '?' + params);
      src.addEventListener('token', e => {
        src.close(); this.oauth.busy = false;
        // Find or create google-workspace MCP server env and set the token
        let gw = this.cfg.mcp_servers.find(s => s.name === 'google-workspace');
        if (!gw) { gw = { name:'google-workspace', command:'uvx', args:['--from','google-workspace-mcp','google-workspace-worker'], env:{} }; this.cfg.mcp_servers.push(gw); }
        gw.env['GOOGLE_WORKSPACE_CLIENT_ID'] = this.oauth.clientId;
        gw.env['GOOGLE_WORKSPACE_CLIENT_SECRET'] = this.oauth.clientSecret;
        gw.env['GOOGLE_WORKSPACE_REFRESH_TOKEN'] = e.data;
        this.banner = { text:'Google Workspace connected! Remember to save.', type:'success' };
      });
      src.addEventListener('error_msg', e => { src.close(); this.oauth.busy = false; this.oauth.error = e.data; });
      src.onerror = () => { src.close(); this.oauth.busy = false; this.oauth.error = 'Connection lost.'; };
    }
  };
}
</script>
{% endblock %}
```

**Step 4: Verify `cargo check` (templates not yet used — syntax check only)**

```bash
cargo check
```

Expected: no errors.

**Step 5: Commit**

```bash
git add templates/
git commit -m "feat(web): add Askama templates (base, chat, config)"
```

---

## Task 3: Create `src/web/` module skeleton

**Files:**
- Create: `src/web/mod.rs`
- Create: `src/web/chat.rs`
- Create: `src/web/config_page.rs`
- Create: `src/web/google_auth.rs`
- Modify: `src/main.rs` — add `mod web;`

**Step 1: Create `src/web/mod.rs`**

```rust
pub mod chat;
pub mod config_page;
pub mod google_auth;

use std::{path::PathBuf, sync::Arc};

use axum::{
    http::StatusCode,
    routing::{get, post},
    Router,
};

use crate::agent::Agent;

/// Shared state for all web handlers.
#[derive(Clone)]
pub struct WebState {
    /// None in setup-only mode; Some in normal mode.
    pub agent: Option<Arc<Agent>>,
    pub config_path: PathBuf,
}

/// Full router for normal mode (chat + config + OAuth).
pub fn build_router(agent: Arc<Agent>, config_path: PathBuf) -> Router {
    let state = WebState {
        agent: Some(agent),
        config_path,
    };
    base_routes(state)
        .route("/", get(chat::page))
        .route("/chat/send", post(chat::send))
        .route("/chat/stream/:session_id", get(chat::stream))
}

/// Minimal router for setup-only mode (config + OAuth only; chat returns 503).
pub fn build_setup_router(config_path: PathBuf) -> Router {
    let state = WebState {
        agent: None,
        config_path,
    };
    base_routes(state)
        .route("/", get(|| async { (StatusCode::SERVICE_UNAVAILABLE, "Bot not running — visit /config to complete setup") }))
        .route("/chat/send", post(|| async { (StatusCode::SERVICE_UNAVAILABLE, axum::Json(serde_json::json!({"error":"Bot not running — complete setup first"}))) }))
}

fn base_routes(state: WebState) -> Router<()> {
    Router::new()
        .route("/config", get(config_page::page))
        .route("/api/load-config", get(config_page::load_config))
        .route("/api/save-config", post(config_page::save_config))
        .route("/api/google-auth/start", post(google_auth::start))
        .route("/api/google-auth/poll/:device_code", get(google_auth::poll))
        .with_state(state)
}
```

**Step 2: Create `src/web/chat.rs`** (placeholder stubs — real implementation in Task 4/6)

```rust
use askama::Template;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{Html, IntoResponse, Sse},
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

use crate::web::WebState;

#[derive(Template)]
#[template(path = "chat.html")]
struct ChatTemplate;

pub async fn page() -> impl IntoResponse {
    Html(ChatTemplate.render().expect("template render"))
}

#[derive(Deserialize)]
pub struct SendRequest {
    pub text: String,
}

#[derive(Serialize)]
pub struct SendResponse {
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub async fn send(
    State(state): State<WebState>,
    Json(body): Json<SendRequest>,
) -> impl IntoResponse {
    let agent = match &state.agent {
        Some(a) => Arc::clone(a),
        None => {
            return Json(SendResponse {
                session_id: String::new(),
                error: Some("Bot not running — complete setup first".into()),
            });
        }
    };

    let text = body.text.trim().to_string();
    if text.is_empty() {
        return Json(SendResponse {
            session_id: String::new(),
            error: Some("empty message".into()),
        });
    }

    let session_id = uuid::Uuid::new_v4().to_string();
    let sid = session_id.clone();

    tokio::spawn(async move {
        let incoming = crate::platform::IncomingMessage {
            platform: "web".into(),
            user_id: "web".into(),
            chat_id: "web".into(),
            user_name: "Web User".into(),
            text,
        };

        match agent.process_message(&incoming).await {
            Ok(response) => {
                for chunk in chunk_string(&response, 4) {
                    let _ = agent.web_tx.send((sid.clone(), chunk));
                    tokio::time::sleep(tokio::time::Duration::from_millis(8)).await;
                }
                let _ = agent.web_tx.send((sid.clone(), "\x00DONE".into()));
            }
            Err(e) => {
                let _ = agent.web_tx.send((sid.clone(), format!("\x00ERR:{e}")));
            }
        }
    });

    Json(SendResponse {
        session_id,
        error: None,
    })
}

pub async fn stream(
    Path(session_id): Path<String>,
    State(state): State<WebState>,
) -> impl IntoResponse {
    use axum::response::sse::Event;

    let agent = match &state.agent {
        Some(a) => Arc::clone(a),
        None => {
            return Sse::new(tokio_stream::once(Ok::<_, std::convert::Infallible>(
                Event::default().event("error").data("Bot not running"),
            )))
            .keep_alive(axum::response::sse::KeepAlive::default());
        }
    };

    let rx = agent.web_tx.subscribe();
    let sid = session_id.clone();

    // Uses scan so the terminal event (done/error) is included before the stream closes.
    let event_stream = BroadcastStream::new(rx)
        .filter_map(move |result| {
            let sid = sid.clone();
            match result {
                Ok((id, token)) if id == sid => {
                    if token == "\x00DONE" {
                        Some(Ok::<_, std::convert::Infallible>((
                            true,
                            Event::default().event("done").data(""),
                        )))
                    } else if let Some(msg) = token.strip_prefix("\x00ERR:") {
                        Some(Ok((true, Event::default().event("error").data(msg.to_string()))))
                    } else {
                        Some(Ok((false, Event::default().event("token").data(token))))
                    }
                }
                _ => None,
            }
        })
        .scan(false, |done, item| {
            if *done {
                return std::future::ready(None);
            }
            match item {
                Ok((is_terminal, event)) => {
                    if is_terminal {
                        *done = true;
                    }
                    std::future::ready(Some(Ok(event)))
                }
                Err(e) => std::future::ready(Some(Err(e))),
            }
        });

    Sse::new(event_stream).keep_alive(axum::response::sse::KeepAlive::default())
}

fn chunk_string(s: &str, chars: usize) -> Vec<String> {
    s.chars()
        .collect::<Vec<_>>()
        .chunks(chars)
        .map(|c| c.iter().collect())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_string_splits_evenly() {
        assert_eq!(chunk_string("abcdef", 2), vec!["ab", "cd", "ef"]);
    }

    #[test]
    fn chunk_string_handles_unicode() {
        assert_eq!(chunk_string("日本語", 1), vec!["日", "本", "語"]);
    }

    #[test]
    fn chunk_string_empty_input() {
        assert!(chunk_string("", 4).is_empty());
    }

    #[test]
    fn chunk_string_shorter_than_chunk_size() {
        assert_eq!(chunk_string("hi", 10), vec!["hi"]);
    }
}
```

**Step 3: Create `src/web/config_page.rs`**

This moves `parse_existing_config`, `format_config`, and related structs from `src/bin/setup.rs`, and adds the Axum handlers. The structs are made `pub` and the tests are ported verbatim.

```rust
use askama::Template;
use axum::{
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse},
    Json,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::web::WebState;

// ── Template ───────────────────────────────────────────────────────────────────

#[derive(Template)]
#[template(path = "config.html")]
struct ConfigTemplate;

pub async fn page() -> impl IntoResponse {
    Html(ConfigTemplate.render().expect("template render"))
}

// ── Load/save config ───────────────────────────────────────────────────────────

pub async fn load_config(State(state): State<WebState>) -> Json<ExistingConfig> {
    match tokio::fs::read_to_string(&state.config_path).await {
        Ok(content) => Json(parse_existing_config(&content)),
        Err(_) => Json(ExistingConfig::default()),
    }
}

#[derive(Deserialize)]
pub struct SaveConfigRequest {
    // All fields from the form; we re-generate TOML from these.
    pub telegram_token: String,
    pub allowed_user_ids: String,
    pub openrouter_key: String,
    pub model: String,
    pub max_tokens: u32,
    pub system_prompt: String,
    pub location: String,
    pub sandbox_dir: String,
    pub db_path: String,
    pub mcp_servers: Vec<McpServerForm>,
}

#[derive(Deserialize)]
pub struct McpServerForm {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

#[derive(Serialize)]
pub struct SaveConfigResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub path: String,
}

pub async fn save_config(
    State(state): State<WebState>,
    Json(body): Json<SaveConfigRequest>,
) -> Result<Json<SaveConfigResponse>, StatusCode> {
    let toml_str = format_config_from_form(&body);
    tokio::fs::write(&state.config_path, &toml_str)
        .await
        .map_err(|e| {
            tracing::error!("Failed to write config: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    let path = state.config_path.to_string_lossy().to_string();
    Ok(Json(SaveConfigResponse {
        ok: true,
        error: None,
        path,
    }))
}

// ── Config data types ──────────────────────────────────────────────────────────

#[derive(Serialize, Default)]
pub struct ExistingConfig {
    pub exists: bool,
    pub telegram_token: String,
    pub allowed_user_ids: String,
    pub openrouter_key: String,
    pub model: String,
    pub max_tokens: u32,
    pub system_prompt: String,
    pub location: String,
    pub sandbox_dir: String,
    pub db_path: String,
    pub mcp_servers: Vec<ExistingMcpServer>,
}

#[derive(Serialize, Default, Clone)]
pub struct ExistingMcpServer {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

// ── Raw TOML structs (loose — all optional so partial configs load) ────────────

#[derive(Deserialize, Default)]
struct RawConfig {
    telegram: Option<RawTelegram>,
    openrouter: Option<RawOpenRouter>,
    sandbox: Option<RawSandbox>,
    memory: Option<RawMemory>,
    general: Option<RawGeneral>,
    #[serde(default)]
    mcp_servers: Vec<RawMcpServer>,
}

#[derive(Deserialize, Default)]
struct RawTelegram {
    bot_token: Option<String>,
    allowed_user_ids: Option<Vec<toml::Value>>,
}

#[derive(Deserialize, Default)]
struct RawOpenRouter {
    api_key: Option<String>,
    model: Option<String>,
    max_tokens: Option<u32>,
    system_prompt: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawSandbox {
    allowed_directory: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawMemory {
    database_path: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawGeneral {
    location: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawMcpServer {
    name: Option<String>,
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: HashMap<String, String>,
}

// ── Parsing ────────────────────────────────────────────────────────────────────

pub fn parse_existing_config(content: &str) -> ExistingConfig {
    let raw: RawConfig = match toml::from_str(content) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("Could not parse existing config.toml: {e}");
            return ExistingConfig::default();
        }
    };

    let tg = raw.telegram.unwrap_or_default();
    let or_ = raw.openrouter.unwrap_or_default();
    let sb = raw.sandbox.unwrap_or_default();
    let mem = raw.memory.unwrap_or_default();

    let allowed_user_ids = tg
        .allowed_user_ids
        .unwrap_or_default()
        .iter()
        .map(|v| match v {
            toml::Value::Integer(i) => i.to_string(),
            toml::Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .collect::<Vec<_>>()
        .join(", ");

    let mcp_servers = raw
        .mcp_servers
        .into_iter()
        .filter_map(|s| {
            let name = s.name.filter(|n| !n.is_empty())?;
            Some(ExistingMcpServer {
                name,
                command: s.command.unwrap_or_default(),
                args: s.args,
                env: s.env,
            })
        })
        .collect();

    ExistingConfig {
        exists: true,
        telegram_token: tg.bot_token.unwrap_or_default(),
        allowed_user_ids,
        openrouter_key: or_.api_key.unwrap_or_default(),
        model: or_.model.unwrap_or_default(),
        max_tokens: or_.max_tokens.unwrap_or(0),
        system_prompt: or_.system_prompt.unwrap_or_default(),
        location: raw.general.as_ref().and_then(|g| g.location.clone()).unwrap_or_default(),
        sandbox_dir: sb.allowed_directory.unwrap_or_default(),
        db_path: mem.database_path.unwrap_or_default(),
        mcp_servers,
    }
}

// ── Formatting ─────────────────────────────────────────────────────────────────

pub fn format_config_from_form(p: &SaveConfigRequest) -> String {
    let ids: Vec<&str> = p
        .allowed_user_ids
        .split([',', ' '])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let ids_str = ids.join(", ");

    let loc_line = if p.location.is_empty() {
        "# location = \"Your City, Country\"".to_owned()
    } else {
        format!("location = \"{}\"", p.location)
    };

    let system_prompt = p.system_prompt.replace('\\', "\\\\").replace('"', "\\\"");
    let max_tokens = p.max_tokens;
    let tg_token = &p.telegram_token;
    let or_key = &p.openrouter_key;
    let model = &p.model;
    let sandbox = &p.sandbox_dir;
    let db_path = &p.db_path;

    let mut out = format!(
        r#"[telegram]
bot_token = "{tg_token}"
allowed_user_ids = [{ids_str}]

[openrouter]
api_key = "{or_key}"
model = "{model}"
base_url = "https://openrouter.ai/api/v1"
max_tokens = {max_tokens}
system_prompt = "{system_prompt}"

[sandbox]
allowed_directory = "{sandbox}"

[memory]
database_path = "{db_path}"

[skills]
directory = "skills"

[general]
{loc_line}
"#
    );

    // Append MCP servers
    for srv in &p.mcp_servers {
        if srv.name.is_empty() {
            continue;
        }
        out.push_str(&format!("\n[[mcp_servers]]\n"));
        out.push_str(&format!("name = \"{}\"\n", srv.name));
        out.push_str(&format!("command = \"{}\"\n", srv.command));
        if !srv.args.is_empty() {
            let args_toml = srv.args.iter().map(|a| format!("\"{}\"", a)).collect::<Vec<_>>().join(", ");
            out.push_str(&format!("args = [{args_toml}]\n"));
        }
        if !srv.env.is_empty() {
            out.push_str(&format!("[mcp_servers.env]\n"));
            for (k, v) in &srv.env {
                out.push_str(&format!("{k} = \"{v}\"\n"));
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_invalid_toml_returns_not_exists() {
        let cfg = parse_existing_config("this is not valid toml !!!");
        assert!(!cfg.exists);
    }

    #[test]
    fn test_parse_full_config() {
        let toml = r#"
[telegram]
bot_token = "mytoken123"
allowed_user_ids = [111, 222]

[openrouter]
api_key = "sk-or-test"
model = "gpt-4o"
max_tokens = 2048
system_prompt = "Be helpful."

[sandbox]
allowed_directory = "/tmp/test"

[memory]
database_path = "test.db"

[general]
location = "Tokyo, Japan"
"#;
        let cfg = parse_existing_config(toml);
        assert!(cfg.exists);
        assert_eq!(cfg.telegram_token, "mytoken123");
        assert_eq!(cfg.allowed_user_ids, "111, 222");
        assert_eq!(cfg.openrouter_key, "sk-or-test");
        assert_eq!(cfg.model, "gpt-4o");
        assert_eq!(cfg.max_tokens, 2048);
        assert_eq!(cfg.system_prompt, "Be helpful.");
        assert_eq!(cfg.location, "Tokyo, Japan");
        assert_eq!(cfg.sandbox_dir, "/tmp/test");
        assert_eq!(cfg.db_path, "test.db");
        assert!(cfg.mcp_servers.is_empty());
    }

    #[test]
    fn test_parse_config_with_mcp_servers() {
        let toml = r#"
[telegram]
bot_token = "t"
allowed_user_ids = [1]
[openrouter]
api_key = "k"
[sandbox]
allowed_directory = "/tmp"
[[mcp_servers]]
name = "git"
command = "uvx"
args = ["mcp-server-git"]
[[mcp_servers]]
name = "brave-search"
command = "npx"
args = ["-y", "@brave/brave-search-mcp-server"]
[mcp_servers.env]
BRAVE_API_KEY = "brave123"
"#;
        let cfg = parse_existing_config(toml);
        assert!(cfg.exists);
        assert_eq!(cfg.mcp_servers.len(), 2);
        assert_eq!(cfg.mcp_servers[0].name, "git");
        assert_eq!(cfg.mcp_servers[0].command, "uvx");
        assert_eq!(cfg.mcp_servers[0].args, vec!["mcp-server-git"]);
        assert!(cfg.mcp_servers[0].env.is_empty());
        assert_eq!(cfg.mcp_servers[1].name, "brave-search");
        assert_eq!(cfg.mcp_servers[1].env.get("BRAVE_API_KEY").unwrap(), "brave123");
    }

    #[test]
    fn test_parse_partial_config_defaults_to_empty() {
        let toml = r#"
[telegram]
bot_token = "partial"
allowed_user_ids = [42]
"#;
        let cfg = parse_existing_config(toml);
        assert!(cfg.exists);
        assert_eq!(cfg.telegram_token, "partial");
        assert_eq!(cfg.model, "");
        assert_eq!(cfg.sandbox_dir, "");
    }

    #[test]
    fn test_parse_string_user_ids() {
        let toml = r#"
[telegram]
bot_token = "t"
allowed_user_ids = ["111", "222"]
[openrouter]
api_key = "k"
[sandbox]
allowed_directory = "/tmp"
"#;
        let cfg = parse_existing_config(toml);
        assert!(cfg.exists);
        assert_eq!(cfg.allowed_user_ids, "111, 222");
    }

    fn make_save_req(tg_token: &str, user_ids: &str, or_key: &str, model: &str, sandbox: &str, db_path: &str, location: &str) -> SaveConfigRequest {
        SaveConfigRequest {
            telegram_token: tg_token.into(),
            allowed_user_ids: user_ids.into(),
            openrouter_key: or_key.into(),
            model: model.into(),
            max_tokens: 4096,
            system_prompt: "Be helpful.".into(),
            location: location.into(),
            sandbox_dir: sandbox.into(),
            db_path: db_path.into(),
            mcp_servers: vec![],
        }
    }

    #[test]
    fn test_format_telegram_section() {
        let out = format_config_from_form(&make_save_req("mytoken", "123456", "k", "m", "/tmp", "d.db", ""));
        assert!(out.contains("[telegram]"));
        assert!(out.contains(r#"bot_token = "mytoken""#));
        assert!(out.contains("allowed_user_ids = [123456]"));
    }

    #[test]
    fn test_format_openrouter_section() {
        let out = format_config_from_form(&make_save_req("t", "1", "sk-or-abc", "gpt-4o", "/tmp", "d.db", ""));
        assert!(out.contains("[openrouter]"));
        assert!(out.contains(r#"api_key = "sk-or-abc""#));
        assert!(out.contains(r#"model = "gpt-4o""#));
        assert!(out.contains("max_tokens = 4096"));
    }

    #[test]
    fn test_format_location_set() {
        let out = format_config_from_form(&make_save_req("t", "1", "k", "m", "/tmp", "d.db", "Tokyo, Japan"));
        assert!(out.contains(r#"location = "Tokyo, Japan""#));
    }

    #[test]
    fn test_format_location_empty_commented() {
        let out = format_config_from_form(&make_save_req("t", "1", "k", "m", "/tmp", "d.db", ""));
        assert!(out.contains("# location ="));
        assert!(!out.contains("\nlocation = "));
    }

    #[test]
    fn test_format_multiple_user_ids() {
        let out = format_config_from_form(&make_save_req("t", "111, 222, 333", "k", "m", "/tmp", "d.db", ""));
        assert!(out.contains("allowed_user_ids = [111, 222, 333]"));
    }
}
```

**Step 4: Create `src/web/google_auth.rs`**

```rust
use axum::{
    extract::{Path, Query, State},
    response::{IntoResponse, Sse},
    Json,
};
use axum::response::sse::Event;
use serde::{Deserialize, Serialize};
use tokio_stream::StreamExt;

use crate::web::WebState;

const GOOGLE_WORKSPACE_SCOPES: &str =
    "https://www.googleapis.com/auth/drive \
     https://www.googleapis.com/auth/gmail.modify \
     https://www.googleapis.com/auth/calendar \
     https://www.googleapis.com/auth/documents \
     https://www.googleapis.com/auth/spreadsheets \
     https://www.googleapis.com/auth/presentations";

// ── Start ──────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct StartRequest {
    pub client_id: String,
    pub client_secret: String,
}

#[derive(Serialize)]
pub struct StartResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub device_code: String,
    pub user_code: String,
    pub verification_url: String,
    pub expires_in: u64,
    pub interval: u64,
}

#[derive(Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_url: String,
    expires_in: u64,
    interval: u64,
}

pub async fn start(
    State(_state): State<WebState>,
    Json(body): Json<StartRequest>,
) -> impl IntoResponse {
    if body.client_id.is_empty() || body.client_secret.is_empty() {
        return Json(StartResponse {
            error: Some("client_id and client_secret are required".into()),
            device_code: String::new(),
            user_code: String::new(),
            verification_url: String::new(),
            expires_in: 0,
            interval: 5,
        });
    }

    let http = reqwest::Client::new();
    let resp = http
        .post("https://oauth2.googleapis.com/device/code")
        .form(&[("client_id", &body.client_id), ("scope", &GOOGLE_WORKSPACE_SCOPES.to_string())])
        .send()
        .await;

    match resp {
        Err(e) => Json(StartResponse {
            error: Some(format!("Failed to contact Google: {e}")),
            device_code: String::new(),
            user_code: String::new(),
            verification_url: String::new(),
            expires_in: 0,
            interval: 5,
        }),
        Ok(r) if !r.status().is_success() => {
            let text = r.text().await.unwrap_or_default();
            Json(StartResponse {
                error: Some(format!("Google error: {text}")),
                device_code: String::new(),
                user_code: String::new(),
                verification_url: String::new(),
                expires_in: 0,
                interval: 5,
            })
        }
        Ok(r) => match r.json::<DeviceCodeResponse>().await {
            Err(e) => Json(StartResponse {
                error: Some(format!("Failed to parse Google response: {e}")),
                device_code: String::new(),
                user_code: String::new(),
                verification_url: String::new(),
                expires_in: 0,
                interval: 5,
            }),
            Ok(d) => Json(StartResponse {
                error: None,
                device_code: d.device_code,
                user_code: d.user_code,
                verification_url: d.verification_url,
                expires_in: d.expires_in,
                interval: d.interval,
            }),
        },
    }
}

// ── Poll SSE ───────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct PollQuery {
    pub client_id: String,
    pub client_secret: String,
    pub interval: Option<u64>,
}

#[derive(Deserialize)]
struct TokenResponse {
    refresh_token: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

pub async fn poll(
    Path(device_code): Path<String>,
    Query(params): Query<PollQuery>,
    State(_state): State<WebState>,
) -> impl IntoResponse {
    let client_id = params.client_id.clone();
    let client_secret = params.client_secret.clone();
    let interval_secs = params.interval.unwrap_or(5).max(5);
    let poll_interval = std::time::Duration::from_secs(interval_secs);
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(1800); // 30 min max

    let stream = async_stream::stream! {
        let http = reqwest::Client::new();
        loop {
            tokio::time::sleep(poll_interval).await;

            if std::time::Instant::now() > deadline {
                yield Ok::<_, std::convert::Infallible>(
                    Event::default().event("error_msg").data("Authorization timed out.")
                );
                break;
            }

            let resp = http
                .post("https://oauth2.googleapis.com/token")
                .form(&[
                    ("client_id", client_id.as_str()),
                    ("client_secret", client_secret.as_str()),
                    ("device_code", device_code.as_str()),
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ])
                .send()
                .await;

            let body: TokenResponse = match resp {
                Err(e) => {
                    yield Ok(Event::default().event("error_msg").data(format!("Network error: {e}")));
                    break;
                }
                Ok(r) => match r.json().await {
                    Err(e) => {
                        yield Ok(Event::default().event("error_msg").data(format!("Parse error: {e}")));
                        break;
                    }
                    Ok(b) => b,
                },
            };

            match body.error.as_deref() {
                None => {
                    match body.refresh_token {
                        Some(rt) => {
                            yield Ok(Event::default().event("token").data(rt));
                            break;
                        }
                        None => {
                            yield Ok(Event::default().event("error_msg").data(
                                "No refresh_token — ensure OAuth client type is 'Desktop app'."
                            ));
                            break;
                        }
                    }
                }
                Some("authorization_pending") => {
                    yield Ok(Event::default().event("pending").data(""));
                }
                Some("slow_down") => {
                    tokio::time::sleep(poll_interval).await;
                    yield Ok(Event::default().event("pending").data(""));
                }
                Some("access_denied") => {
                    yield Ok(Event::default().event("error_msg").data("Authorization was denied."));
                    break;
                }
                Some("expired_token") => {
                    yield Ok(Event::default().event("error_msg").data("Device code expired. Try again."));
                    break;
                }
                Some(other) => {
                    let desc = body.error_description.as_deref().unwrap_or("");
                    yield Ok(Event::default().event("error_msg").data(format!("{other}: {desc}")));
                    break;
                }
            }
        }
    };

    Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default())
}
```

Note: `async_stream` needs to be added to `Cargo.toml` — add `async-stream = "0.3"` in the dependencies.

**Step 5: Add `mod web;` to `src/main.rs`**

Add `mod web;` after the other `mod` declarations at the top of `src/main.rs`.

**Step 6: Run `cargo check`**

```bash
cargo check
```

Expected: compiles. The `web_tx` field on `Agent` doesn't exist yet — if the compiler errors on `agent.web_tx`, note the exact error. `chat.rs` references it, so expect one error there.

**Step 7: Commit the skeleton**

```bash
git add src/web/ src/main.rs
git commit -m "feat(web): add web module skeleton (chat, config_page, google_auth)"
```

---

## Task 4: Add `async-stream` dependency and `web_tx` to `Agent`

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/agent.rs`

**Step 1: Add `async-stream` to `Cargo.toml`**

After the `async-trait = "0.1"` line:

```toml
async-stream = "0.3"
```

Also update the axum comment since it's no longer setup-only:

Change `# Setup wizard web server (used only by src/bin/setup.rs)` to `# Web server`.

**Step 2: Add `web_tx` field to `Agent` struct** (in `src/agent.rs`, after `job_tx`):

```rust
/// Broadcast channel for web UI token streaming.
/// Each message is (session_id, token). Sentinels: "\x00DONE", "\x00ERR:…"
pub web_tx: Arc<tokio::sync::broadcast::Sender<(String, String)>>,
```

Add `use std::sync::Arc;` if not already imported (check existing imports — `Weak` is already used, so `Arc` should already be in scope via `std::sync`).

**Step 3: Update `Agent::new` to create and store `web_tx`**

At the top of `Agent::new`, before `let llm = ...`:

```rust
let (web_tx_inner, _) = tokio::sync::broadcast::channel(256);
let web_tx = Arc::new(web_tx_inner);
```

Add `web_tx` to the `Self { ... }` struct literal:

```rust
Self {
    llm,
    config,
    mcp,
    memory,
    skills: tokio::sync::RwLock::new(skills),
    task_store,
    scheduler,
    bot,
    self_weak,
    job_tx,
    web_tx,      // ← add this
}
```

**Step 4: Run `cargo check`**

```bash
cargo check
```

Expected: no errors. The `#[allow(dead_code)]` warning on `self_weak` may remain — that's fine.

**Step 5: Commit**

```bash
git add Cargo.toml src/agent.rs
git commit -m "feat(web): add web_tx broadcast channel to Agent and async-stream dep"
```

---

## Task 5: Update `main.rs` — detect mode and spawn web server

**Files:**
- Modify: `src/main.rs`

Replace the current `main()` function with a version that:
1. Tries to load config.
2. If load fails → runs `run_setup_mode()`.
3. If load succeeds → runs `run_normal_mode()`.

**Step 1: Rewrite `src/main.rs`**

```rust
mod agent;
mod config;
mod llm;
mod mcp;
mod memory;
mod platform;
mod scheduler;
mod skills;
mod tools;
mod web;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use crate::agent::Agent;
use crate::config::Config;
use crate::mcp::McpManager;
use crate::memory::MemoryStore;
use crate::scheduler::tasks::register_builtin_tasks;
use crate::scheduler::Scheduler;
use crate::skills::loader::load_skills_from_dir;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,rustfox=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("config.toml"));

    match Config::load(&config_path) {
        Ok(config) => run_normal_mode(config, config_path).await,
        Err(e) => {
            tracing::warn!("Config load failed ({e:#}). Starting setup-only mode.");
            run_setup_mode(config_path).await
        }
    }
}

async fn run_setup_mode(config_path: PathBuf) -> Result<()> {
    let port: u16 = std::env::var("RUSTFOX_SETUP_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8080);

    info!("No valid config found — starting setup server on :{port}");
    info!("Open http://localhost:{port}/config to configure the bot.");

    let router = crate::web::build_setup_router(config_path);
    let addr = format!("127.0.0.1:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("Failed to bind setup server to {addr}"))?;

    axum::serve(listener, router)
        .await
        .context("Setup server error")?;

    Ok(())
}

async fn run_normal_mode(config: Config, config_path: PathBuf) -> Result<()> {
    info!("Configuration loaded successfully");
    info!("  Model: {}", config.openrouter.model);
    info!("  Sandbox: {}", config.sandbox.allowed_directory.display());
    info!("  Allowed users: {:?}", config.telegram.allowed_user_ids);
    info!("  MCP servers: {}", config.mcp_servers.len());

    let embedding_config = config.embedding.as_ref().map(|cfg| {
        crate::memory::embeddings::EmbeddingConfig {
            api_key: cfg.api_key.clone(),
            base_url: cfg.base_url.clone(),
            model: cfg.model.clone(),
            dimensions: cfg.dimensions,
        }
    });

    let memory = MemoryStore::open(&config.memory.database_path, embedding_config)
        .context("Failed to initialize memory store")?;
    info!("  Database: {}", config.memory.database_path.display());

    let mut mcp_manager = McpManager::new();
    mcp_manager.connect_all(&config.mcp_servers).await;

    let skills = load_skills_from_dir(&config.skills.directory).await?;
    info!("  Skills: {}", skills.len());

    let task_store =
        crate::scheduler::reminders::ScheduledTaskStore::new(memory.connection());
    let scheduler = Arc::new(Scheduler::new().await?);
    let bot = Arc::new(teloxide::Bot::new(&config.telegram.bot_token));

    let (job_tx, mut job_rx) =
        tokio::sync::mpsc::unbounded_channel::<crate::agent::ScheduledJobRequest>();

    let agent = Arc::new_cyclic(|weak| {
        Agent::new(
            config.clone(),
            mcp_manager,
            memory.clone(),
            skills,
            task_store.clone(),
            Arc::clone(&scheduler),
            Arc::clone(&bot),
            weak.clone(),
            job_tx,
        )
    });

    // Background scheduled job runner
    let agent_for_runner = Arc::clone(&agent);
    tokio::spawn(async move {
        use teloxide::prelude::*;
        while let Some(req) = job_rx.recv().await {
            let agent = Arc::clone(&agent_for_runner);
            if !req.is_recurring {
                let _ = req.task_store.set_status(&req.task_id, "completed").await;
            }
            let response = match agent.process_message(&req.incoming).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!("Scheduled task {} failed: {}", req.task_id, e);
                    if !req.is_recurring {
                        let _ = req.task_store.set_status(&req.task_id, "failed").await;
                    }
                    continue;
                }
            };
            let chat_id_val: i64 = match req.incoming.chat_id.parse() {
                Ok(v) => v,
                Err(_) => {
                    tracing::error!(
                        "Unparseable chat_id '{}' for task {}",
                        req.incoming.chat_id,
                        req.task_id
                    );
                    continue;
                }
            };
            let chat = teloxide::types::ChatId(chat_id_val);
            for chunk in crate::agent::split_response_chunks(&response, 4000) {
                if chunk.is_empty() {
                    continue;
                }
                if let Err(e) = req.bot.send_message(chat, &chunk).await {
                    tracing::error!("Failed to send scheduled response: {}", e);
                }
            }
        }
    });

    register_builtin_tasks(&scheduler, memory).await?;
    scheduler.start().await?;
    info!("  Scheduler: active");
    agent.restore_scheduled_tasks().await;
    info!("  Scheduled tasks: restored from DB");

    // Spawn web server if enabled
    if config.web.enabled {
        let web_agent = Arc::clone(&agent);
        let web_port = config.web.port;
        let web_config_path = config_path.clone();
        tokio::spawn(async move {
            let router = crate::web::build_router(web_agent, web_config_path);
            let addr = format!("127.0.0.1:{web_port}");
            match tokio::net::TcpListener::bind(&addr).await {
                Ok(listener) => {
                    info!("Web UI listening on http://127.0.0.1:{web_port}");
                    if let Err(e) = axum::serve(listener, router).await {
                        tracing::error!("Web server error: {e}");
                    }
                }
                Err(e) => tracing::error!("Web server failed to bind {addr}: {e}"),
            }
        });
    }

    info!("Bot is starting...");
    platform::telegram::run(
        agent,
        config.telegram.allowed_user_ids.clone(),
        Arc::clone(&bot),
    )
    .await?;

    Ok(())
}
```

**Step 2: Run `cargo check`**

```bash
cargo check
```

Expected: no errors.

**Step 3: Run `cargo clippy -- -D warnings`**

```bash
cargo clippy -- -D warnings
```

Fix any warnings before continuing.

**Step 4: Run `cargo fmt`**

```bash
cargo fmt
```

**Step 5: Commit**

```bash
git add src/main.rs
git commit -m "feat(web): setup-only mode + web server spawn in main.rs"
```

---

## Task 6: Delete `src/bin/setup.rs`

Now that all logic has moved to `src/web/`, the setup binary is no longer needed.

**Files:**
- Delete: `src/bin/setup.rs`
- Delete: `setup/index.html` (the embedded SPA, no longer needed)
- Delete: `setup/` directory (if empty after removing index.html)
- Modify: `Cargo.toml` — update the axum comment (done in Task 4)

**Step 1: Delete the setup binary and embedded HTML**

```bash
rm src/bin/setup.rs
rm -rf setup/
```

Since there are no explicit `[[bin]]` entries in `Cargo.toml`, Cargo auto-discovers bins. Removing `src/bin/setup.rs` automatically removes the `setup` binary.

**Step 2: Run `cargo check`**

```bash
cargo check
```

Expected: no errors. `cargo build` should no longer produce a `setup` binary.

**Step 3: Run `cargo test`**

```bash
cargo test
```

Expected: all tests pass. The tests from `setup.rs` now live in `src/web/config_page.rs` (ported in Task 3). Verify test count includes the config_page tests.

**Step 4: Commit**

```bash
git add -A
git commit -m "chore: delete setup binary (logic moved to src/web/config_page.rs)"
```

---

## Task 7: Final CI checks and push

**Step 1: Run full CI suite**

```bash
cargo fmt --all -- --check
```

Expected: no formatting issues.

```bash
cargo clippy -- -D warnings
```

Expected: no warnings. Common issues to fix:
- `dead_code` on `WebState.agent` → add `#[allow(dead_code)]` only if truly unreachable; better to use it in all handlers.
- Unused imports → remove them.

```bash
cargo test
```

Expected: all tests pass, including:
- `web::config_page::tests::*` (8 tests)
- `web::chat::tests::*` (4 tests)
- Any pre-existing tests.

```bash
cargo build --release
```

Expected: release build succeeds.

**Step 2: Push to branch**

```bash
git push -u origin claude/fix-workspace-token-refresh-jLR6R
```

---

## Reference: Key Types

```rust
// WebState — shared across all web handlers
pub struct WebState {
    pub agent: Option<Arc<Agent>>,  // None = setup-only mode
    pub config_path: PathBuf,
}

// Agent gains:
pub web_tx: Arc<tokio::sync::broadcast::Sender<(String, String)>>,

// SSE sentinel values in web_tx messages:
// "\x00DONE"      → done event, stream ends
// "\x00ERR:..."   → error event, stream ends
// anything else   → token event
```

## Reference: Route Summary

| Method | Path | File | Mode |
|--------|------|------|------|
| GET | `/` | `web/chat.rs::page` | Normal |
| GET | `/config` | `web/config_page.rs::page` | Both |
| POST | `/chat/send` | `web/chat.rs::send` | Normal |
| GET | `/chat/stream/:id` | `web/chat.rs::stream` | Normal |
| GET | `/api/load-config` | `web/config_page.rs::load_config` | Both |
| POST | `/api/save-config` | `web/config_page.rs::save_config` | Both |
| POST | `/api/google-auth/start` | `web/google_auth.rs::start` | Both |
| GET | `/api/google-auth/poll/:dc` | `web/google_auth.rs::poll` | Both |
