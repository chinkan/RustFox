# Setup Wizard Redesign — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Redesign the setup wizard with progressive disclosure — non-tech users see only essentials, tech users can expand to all ~30 config settings.

**Architecture:** Rust `axum` backend (`src/bin/setup.rs`) serves a single-page HTML wizard (`setup/index.html`). Wizard POSTs JSON config to the backend. Both web UI and CLI terminal wizard (`--cli` flag) need updating.

**Tech Stack:** Rust (axum, tokio, serde, toml), HTML/CSS/JS (vanilla, no frameworks)

**Spec:** `docs/superpowers/specs/2026-06-11-setup-wizard-redesign.md`

---

## File Structure

| File | Changes |
|------|---------|
| `src/bin/setup.rs` | Add new Raw* parse structs, update ExistingConfig + load_config, update save_config to serialize all sections |
| `setup/index.html` | Full rewrite: progressive disclosure layout, collapsible advanced sections, global toggle, raw TOML preview |

### Task 1: Add new parse structs to setup.rs

**Files:**
- Modify: `src/bin/setup.rs`

- [ ] **Step 1: Add new Raw* structs**

After the existing `RawGeneral` struct (around line 161), add:

```rust
#[derive(Deserialize, Default)]
struct RawAgent {
    max_iterations: Option<u32>,
    empty_response_retry_limit: Option<u32>,
}

#[derive(Deserialize, Default)]
struct RawLangSmith {
    api_key: Option<String>,
    project: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawEmbedding {
    api_key: Option<String>,
    base_url: Option<String>,
    model: Option<String>,
    dimensions: Option<u32>,
}

#[derive(Deserialize, Default)]
struct RawOcr {
    model_dir: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawLearning {
    user_model_path: Option<String>,
    skill_extraction_enabled: Option<bool>,
    skill_extraction_threshold: Option<u32>,
    user_model_update_interval: Option<u32>,
    user_model_cron: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawSkills {
    directory: Option<String>,
}
```

- [ ] **Step 2: Add new fields to `RawConfig`**

```rust
#[derive(Deserialize, Default)]
struct RawConfig {
    telegram: Option<RawTelegram>,
    openrouter: Option<RawOpenRouter>,
    sandbox: Option<RawSandbox>,
    memory: Option<RawMemory>,
    general: Option<RawGeneral>,
    agent: Option<RawAgent>,
    langsmith: Option<RawLangSmith>,
    embedding: Option<RawEmbedding>,
    ocr: Option<RawOcr>,
    learning: Option<RawLearning>,
    skills: Option<RawSkills>,
    #[serde(default)]
    mcp_servers: Vec<RawMcpServer>,
}
```

- [ ] **Step 3: Add new fields to `ExistingConfig`**

```rust
#[derive(Serialize, Default)]
struct ExistingConfig {
    exists: bool,
    telegram_token: String,
    allowed_user_ids: String,
    openrouter_key: String,
    model: String,
    max_tokens: u32,
    system_prompt: String,
    location: String,
    sandbox_dir: String,
    db_path: String,
    // New fields
    supports_vision: bool,
    base_url: String,
    home_dir: String,
    skills_dir: String,
    agents_dir: String,
    ocr_model_dir: String,
    agent_max_iterations: u32,
    agent_empty_response_retry_limit: u32,
    langsmith_key: String,
    langsmith_project: String,
    embedding_key: String,
    embedding_base_url: String,
    embedding_model: String,
    embedding_dimensions: u32,
    query_rewriter_enabled: bool,
    learning_skill_extraction_enabled: bool,
    learning_skill_extraction_threshold: u32,
    learning_user_model_update_interval: u32,
    learning_user_model_cron: String,
    mcp_servers: Vec<ExistingMcpServer>,
}
```

- [ ] **Step 4: Update `load_config()` to populate new fields**

In the `load_config` handler (around line 338), add code to populate each new field from the raw parse structs. Pattern:

```rust
if let Some(ref openrouter) = raw.openrouter {
    cfg.supports_vision = openrouter.supports_vision.unwrap_or(false);
    cfg.base_url = openrouter.base_url.clone().unwrap_or_default();
}
// ... repeat for each new section
```

- [ ] **Step 5: Build and verify**

Run: `cargo build`
Expected: compiles without errors

### Task 2: Update save_config to write all sections

**Files:**
- Modify: `src/bin/setup.rs` — `save_config` handler

- [ ] **Step 1: Expand the config serialization**

Current `save_config` receives a raw TOML string and writes it directly. Keep this approach — the form builds the TOML string on the frontend. Just ensure the backend accepts and persists the full config.

The current code:
```rust
async fn save_config(
    State(state): State<AppState>,
    Form(form): Form<SaveRequest>,
) -> Json<SaveResponse> {
    let path = &state.config_path;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.unwrap();
    }
    tokio::fs::write(path, &form.config).await.unwrap();
    Json(SaveResponse { ok: true, path: path.to_string_lossy().to_string() })
}
```

This is fine — the backend just persists whatever TOML the frontend sends. No changes needed for the save endpoint itself.

- [ ] **Step 2: Build and verify**

Run: `cargo build`
Expected: compiles without errors

### Task 3: Rewrite setup/index.html with progressive disclosure

**Files:**
- Modify: `setup/index.html` (full rewrite)

- [ ] **Step 1: New HTML structure**

```html
<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>RustFox Setup</title>
  <style>
    /* Keep existing dark theme, add: */
    /* - Step indicator in sidebar */
    /* - Collapsible .advanced sections */
    /* - "Show all settings" toggle */
    /* - Raw TOML preview styling */
  </style>
</head>
<body>
  <!-- Global toggle -->
  <div class="global-toggle">
    <label><input type="checkbox" id="showAll"> ⚙️ Show all settings</label>
  </div>

  <!-- Step navigation -->
  <div class="steps-nav">
    <div class="step-indicator active" data-step="1">1. Telegram Bot</div>
    <div class="step-indicator" data-step="2">2. Location & Storage</div>
    <div class="step-indicator" data-step="3">3. Integrations</div>
    <div class="step-indicator" data-step="4">4. Review & Save</div>
  </div>

    <!-- Step 1: Bot & LLM Setup -->
  <div class="step active" data-step="1">
    <h2>🤖 Bot & LLM Setup</h2>
    <div class="field">
      <label>Bot Token <span class="hint">(required — from @BotFather)</span></label>
      <input type="password" id="botToken" placeholder="123456:ABC-def..." required>
      <div class="error-msg">Bot token is required</div>
    </div>
    <div class="field">
      <label>User IDs <span class="hint">(required — comma-separated)</span></label>
      <input type="text" id="userIds" placeholder="123456789, 987654321" required>
      <div class="error-msg">At least one user ID is required</div>
    </div>
    <div class="field">
      <label>OpenRouter API Key <span class="hint">(required)</span></label>
      <input type="password" id="openrouterKey" placeholder="sk-or-v1-..." required>
      <div class="error-msg">API key is required</div>
    </div>
    <div class="advanced-section">
      <button class="advanced-toggle" type="button">▼ Advanced LLM settings</button>
      <div class="advanced-content">
        <div class="field">
          <label>System prompt</label>
          <textarea id="systemPrompt" placeholder="You are a helpful AI assistant...">You are a helpful AI assistant with access to tools...</textarea>
        </div>
        <div class="field">
          <label>Model</label>
          <input type="text" id="model" placeholder="moonshotai/kimi-k2.5" value="moonshotai/kimi-k2.5">
        </div>
        <div class="field-row">
          <div class="field">
            <label>Max tokens</label>
            <input type="number" id="maxTokens" value="4096" min="1">
          </div>
          <div class="field">
            <label>Supports vision</label>
            <input type="checkbox" id="supportsVision">
          </div>
        </div>
        <div class="field">
          <label>API Base URL</label>
          <input type="text" id="baseUrl" placeholder="https://openrouter.ai/api/v1" value="https://openrouter.ai/api/v1">
        </div>
      </div>
    </div>
    <div class="btn-row">
      <button class="btn-primary" onclick="nextStep(1)">Next →</button>
    </div>
  </div>

  <!-- Step 2: Location & Storage -->
  <div class="step" data-step="2">
    <h2>🌍 Location & Storage</h2>
    <div class="field">
      <label>Your location <span class="hint">(so the AI knows your timezone)</span></label>
      <input type="text" id="location" placeholder="Tokyo, Japan">
    </div>
    <div class="advanced-section">
      <button class="advanced-toggle" type="button">▼ Advanced settings</button>
      <div class="advanced-content">
        <div class="field">
          <label>Sandbox directory</label>
          <input type="text" id="sandboxDir" placeholder="~/.rustfox/workspace">
        </div>
        <div class="field">
          <label>Database path</label>
          <input type="text" id="dbPath" placeholder="~/.rustfox/rustfox.db">
        </div>
        <div class="field">
          <label>Skills directory</label>
          <input type="text" id="skillsDir" placeholder="~/.rustfox/skills">
        </div>
        <div class="field">
          <label>Agents directory</label>
          <input type="text" id="agentsDir" placeholder="~/.rustfox/agents">
        </div>
        <div class="field">
          <label>Home directory</label>
          <input type="text" id="homeDir" placeholder="~/.rustfox">
        </div>
        <div class="field">
          <label>OCR model directory</label>
          <input type="text" id="ocrModelDir" placeholder="~/.cache/ocrs">
        </div>
        <div class="field">
          <label>Query rewriting</label>
          <input type="checkbox" id="queryRewriter"> Enable RAG query rewriting
        </div>
        <h3>🤖 Agent Loop</h3>
        <div class="field-row">
          <div class="field">
            <label>Max iterations</label>
            <input type="number" id="agentMaxIterations" value="25" min="1">
          </div>
          <div class="field">
            <label>Empty response retry limit</label>
            <input type="number" id="agentEmptyRetry" value="3" min="0">
          </div>
        </div>
      </div>
    </div>
    <div class="btn-row">
      <button class="btn-secondary" onclick="prevStep(2)">← Back</button>
      <button class="btn-primary" onclick="nextStep(2)">Next →</button>
    </div>
  </div>

  <!-- Step 3: Integrations -->
  <div class="step" data-step="3">
    <h2>🧩 Integrations</h2>
    <div class="mcp-category">
      <h3>MCP Servers</h3>
      <!-- Keep existing JS-driven MCP catalog rendering (MCP_CATALOG, buildMCPCatalog(), etc.) -->
      <!-- The existing wizard's MCP selection code is preserved unchanged:
           - MCP_CATALOG array with all server definitions
           - renderMcpCatalog() to render the checkboxes
           - selectedMcpServers global array for buildToml() to read
           - OAuth popup flow for HTTP MCP servers (Notion, etc.)
           The only change is moving the container div to Step 3. -->
      <div id="mcpCatalogContainer"></div>
    </div>
    <div class="advanced-section">
      <button class="advanced-toggle" type="button">▼ Advanced settings</button>
      <div class="advanced-content">
        <h3>📊 LangSmith (optional)</h3>
        <div class="field">
          <label>API Key</label>
          <input type="password" id="langsmithKey" placeholder="ls__...">
        </div>
        <div class="field">
          <label>Project name</label>
          <input type="text" id="langsmithProject" placeholder="rustfox" value="rustfox">
        </div>
        <h3>🔍 Embedding API (optional)</h3>
        <div class="field">
          <label>API Key</label>
          <input type="password" id="embeddingKey" placeholder="sk-...">
        </div>
        <div class="field">
          <label>Base URL</label>
          <input type="text" id="embeddingBaseUrl" placeholder="https://openrouter.ai/api/v1" value="https://openrouter.ai/api/v1">
        </div>
        <div class="field">
          <label>Model</label>
          <input type="text" id="embeddingModel" placeholder="qwen/qwen3-embedding-8b" value="qwen/qwen3-embedding-8b">
        </div>
        <div class="field">
          <label>Dimensions</label>
          <input type="number" id="embeddingDimensions" value="1536" min="1">
        </div>
        <h3>🧠 Learning (optional)</h3>
        <div class="field">
          <label>Skill extraction enabled</label>
          <input type="checkbox" id="learningExtraction" checked>
        </div>
        <div class="field">
          <label>Extraction threshold <span class="hint">(min tool calls)</span></label>
          <input type="number" id="learningThreshold" value="5" min="1">
        </div>
      </div>
    </div>
    <div class="btn-row">
      <button class="btn-secondary" onclick="prevStep(3)">← Back</button>
      <button class="btn-primary" onclick="nextStep(3)">Next →</button>
    </div>
  </div>

  <!-- Step 4: Review & Save -->
  <div class="step" data-step="4">
    <h2>✅ Review & Save</h2>
    <div id="summary"></div>
    <button class="advanced-toggle" type="button" onclick="toggleRawToml()">📋 Show raw TOML</button>
    <pre id="rawToml" style="display:none"></pre>
    <div class="btn-row">
      <button class="btn-secondary" onclick="prevStep(4)">← Back</button>
      <button class="btn-primary" onclick="saveConfig()">💾 Save config</button>
    </div>
  </div>

  <!-- Step 5: Success -->
  <div class="step" data-step="5">
    <h2>✅ Config saved!</h2>
    <p>Your RustFox configuration has been written. Start the bot with:</p>
    <pre>cargo run --bin rustfox</pre>
  </div>
</body>
</html>
```

- [ ] **Step 2: Add the JavaScript logic**

Key JS functions. Use the existing `esc()` function (already defined in the current wizard) instead of a new `escapeToml()`:

```javascript
// Step navigation
let currentStep = 1;
const TOTAL_STEPS = 4;

function validateStep(step) {
  const fields = {
    1: [
      { id: 'botToken', msg: 'Bot token is required' },
      { id: 'userIds', msg: 'At least one user ID is required' },
      { id: 'openrouterKey', msg: 'API key is required' },
    ],
  };
  const checks = fields[step] || [];
  let valid = true;
  checks.forEach(f => {
    const el = document.getElementById(f.id);
    const err = el.parentElement.querySelector('.error-msg');
    if (!el.value.trim()) {
      el.classList.add('error');
      if (err) err.classList.add('visible');
      valid = false;
    } else {
      el.classList.remove('error');
      if (err) err.classList.remove('visible');
    }
  });
  return valid;
}

function showStep(n) {
  document.querySelectorAll('.step').forEach(s => s.classList.remove('active'));
  document.querySelector(`.step[data-step="${n}"]`).classList.add('active');
  document.querySelectorAll('.step-indicator').forEach(s => s.classList.remove('active'));
  document.querySelector(`.step-indicator[data-step="${n}"]`).classList.add('active');
  currentStep = n;
}

function nextStep(step) {
  if (!validateStep(step)) return;
  showStep(step + 1);
}

function prevStep(step) { showStep(step - 1); }

// Global "Show all settings" toggle
document.getElementById('showAll').addEventListener('change', function() {
  document.querySelectorAll('.advanced-content').forEach(el => {
    el.style.display = this.checked ? 'block' : 'none';
  });
  localStorage.setItem('setup_showAll', this.checked);
});

// Advanced section toggles
document.querySelectorAll('.advanced-toggle').forEach(btn => {
  btn.addEventListener('click', function() {
    const content = this.nextElementSibling;
    const isVisible = content.style.display !== 'none';
    content.style.display = isVisible ? 'none' : 'block';
    this.textContent = isVisible ? '▶ Advanced' : '▼ Advanced';
  });
});

// Load existing config on page load
async function loadExistingConfig() {
  const resp = await fetch('/api/load-config');
  const data = await resp.json();
  if (!data.exists) return;
  document.getElementById('botToken').value = data.telegram_token || '';
  document.getElementById('userIds').value = data.allowed_user_ids || '';
  document.getElementById('openrouterKey').value = data.openrouter_key || '';
  document.getElementById('model').value = data.model || 'moonshotai/kimi-k2.5';
  document.getElementById('maxTokens').value = data.max_tokens || 4096;
  document.getElementById('systemPrompt').value = data.system_prompt || '';
  document.getElementById('location').value = data.location || '';
  document.getElementById('sandboxDir').value = data.sandbox_dir || '';
  document.getElementById('dbPath').value = data.db_path || '';
  document.getElementById('homeDir').value = data.home_dir || '';
  document.getElementById('skillsDir').value = data.skills_dir || '';
  document.getElementById('agentsDir').value = data.agents_dir || '';
  document.getElementById('ocrModelDir').value = data.ocr_model_dir || '';
  document.getElementById('baseUrl').value = data.base_url || 'https://openrouter.ai/api/v1';
  document.getElementById('supportsVision').checked = data.supports_vision || false;
  document.getElementById('queryRewriter').checked = data.query_rewriter_enabled || false;
  document.getElementById('agentMaxIterations').value = data.agent_max_iterations || 25;
  document.getElementById('agentEmptyRetry').value = data.agent_empty_response_retry_limit || 3;
  document.getElementById('langsmithKey').value = data.langsmith_key || '';
  document.getElementById('langsmithProject').value = data.langsmith_project || 'rustfox';
  document.getElementById('embeddingKey').value = data.embedding_key || '';
  document.getElementById('embeddingBaseUrl').value = data.embedding_base_url || '';
  document.getElementById('embeddingModel').value = data.embedding_model || 'qwen/qwen3-embedding-8b';
  document.getElementById('embeddingDimensions').value = data.embedding_dimensions || 1536;
  document.getElementById('learningExtraction').checked = data.learning_skill_extraction_enabled !== false;
  document.getElementById('learningThreshold').value = data.learning_skill_extraction_threshold || 5;
  // MCP servers populated by existing JS (keep current mcp-server rendering logic)
}

// Helper: escape special characters for TOML string values
function esc(s) { return s.replace(/\\/g, '\\\\').replace(/"/g, '\\"').replace(/\n/g, '\\n'); }

// Build TOML from form fields — all sections
function buildToml() {
  let t = '# RustFox Configuration\n# Generated by setup wizard\n\n';

  // [telegram]
  t += `[telegram]\nbot_token = "${esc(document.getElementById('botToken').value)}"\n`;
  t += `allowed_user_ids = [${document.getElementById('userIds').value.split(',').map(s => s.trim()).filter(Boolean).join(', ')}]\n\n`;

  // [openrouter]
  t += `[openrouter]\napi_key = "${esc(document.getElementById('openrouterKey').value)}"\n`;
  const model = document.getElementById('model').value || 'moonshotai/kimi-k2.5';
  t += `model = "${esc(model)}"\n`;
  const baseUrl = document.getElementById('baseUrl').value || 'https://openrouter.ai/api/v1';
  t += `base_url = "${esc(baseUrl)}"\n`;
  t += `max_tokens = ${document.getElementById('maxTokens').value || 4096}\n`;
  if (document.getElementById('supportsVision').checked) t += 'supports_vision = true\n';
  const sp = document.getElementById('systemPrompt').value;
  if (sp) t += `system_prompt = """${sp}"""\n\n`;

  // [sandbox]
  const sd = document.getElementById('sandboxDir').value;
  if (sd) t += `[sandbox]\nallowed_directory = "${esc(sd)}"\n\n`;

  // [memory]
  t += `[memory]\n`;
  const dbp = document.getElementById('dbPath').value;
  if (dbp) t += `database_path = "${esc(dbp)}"\n`;
  if (document.getElementById('queryRewriter').checked) t += 'query_rewriter_enabled = true\n';
  t += '\n';

  // [skills] + [agents]
  const skd = document.getElementById('skillsDir').value;
  if (skd) t += `[skills]\ndirectory = "${esc(skd)}"\n\n`;
  const agd = document.getElementById('agentsDir').value;
  if (agd) t += `[agents]\ndirectory = "${esc(agd)}"\n\n`;

  // [general]
  t += `[general]\n`;
  const homed = document.getElementById('homeDir').value;
  if (homed) t += `home = "${esc(homed)}"\n`;
  const loc = document.getElementById('location').value;
  if (loc) t += `location = "${esc(loc)}"\n`;
  t += '\n';

  // [agent]
  t += `[agent]\n`;
  t += `max_iterations = ${document.getElementById('agentMaxIterations').value || 25}\n`;
  t += `empty_response_retry_limit = ${document.getElementById('agentEmptyRetry').value || 3}\n\n`;

  // [langsmith] (optional)
  const lsk = document.getElementById('langsmithKey').value;
  if (lsk) {
    t += `[langsmith]\napi_key = "${esc(lsk)}"\n`;
    t += `project = "${esc(document.getElementById('langsmithProject').value || 'rustfox')}"\n\n`;
  }

  // [embedding] (optional)
  const esk = document.getElementById('embeddingKey').value;
  if (esk) {
    t += `[embedding]\napi_key = "${esc(esk)}"\n`;
    t += `base_url = "${esc(document.getElementById('embeddingBaseUrl').value || 'https://openrouter.ai/api/v1')}"\n`;
    t += `model = "${esc(document.getElementById('embeddingModel').value || 'qwen/qwen3-embedding-8b')}"\n`;
    t += `dimensions = ${document.getElementById('embeddingDimensions').value || 1536}\n\n`;
  }

  // [ocr] (optional)
  const ocrd = document.getElementById('ocrModelDir').value;
  if (ocrd) t += `[ocr]\nmodel_dir = "${esc(ocrd)}"\n\n`;

  // [learning] (optional)
  if (document.getElementById('learningExtraction').checked) {
    t += `[learning]\nskill_extraction_enabled = true\n`;
    t += `skill_extraction_threshold = ${document.getElementById('learningThreshold').value || 5}\n\n`;
  }

  // [[mcp_servers]] — populated by existing JS MCP catalog rendering
  // The existing JS collects selected MCP servers into a global array.
  // Append them here using the existing format.
  if (typeof selectedMcpServers !== 'undefined') {
    selectedMcpServers.forEach(srv => {
      t += `[[mcp_servers]]\nname = "${esc(srv.name)}"\n`;
      if (srv.command) t += `command = "${esc(srv.command)}"\n`;
      if (srv.args && srv.args.length) t += `args = [${srv.args.map(a => `"${esc(a)}"`).join(', ')}]\n`;
      if (srv.url) t += `url = "${esc(srv.url)}"\n`;
      if (srv.auth_token) t += `auth_token = "${esc(srv.auth_token)}"\n`;
      if (srv.env && Object.keys(srv.env).length) {
        t += `[mcp_servers.env]\n`;
        for (const [k, v] of Object.entries(srv.env)) {
          t += `${k} = "${esc(v)}"\n`;
        }
      }
      t += '\n';
    });
  }

  return t;
}

// Save config
async function saveConfig() {
  const toml = buildToml();
  document.getElementById('rawToml').textContent = toml;
  const form = new FormData();
  form.append('config', toml);
  const resp = await fetch('/save-config', { method: 'POST', body: form });
  const data = await resp.json();
  if (data.ok) {
    showStep(5); // success page
    document.querySelector('.step[data-step="5"] h2').textContent = `✅ Saved to ${data.path}`;
  }
}

function toggleRawToml() {
  const el = document.getElementById('rawToml');
  const toml = buildToml();
  el.textContent = toml;
  el.style.display = el.style.display === 'none' ? 'block' : 'none';
}

// Load existing config on page load
loadExistingConfig();
```

- [ ] **Step 3: Add CSS for progressive disclosure**

```css
/* Advanced section toggle */
.advanced-section { margin-top: 1.5rem; border-top: 1px solid #2d3748; padding-top: 1rem; }
.advanced-toggle {
  background: none; border: 1px solid #2d3748; border-radius: 6px;
  color: #718096; cursor: pointer; font-size: 0.8rem; padding: 0.3rem 0.75rem;
  margin-bottom: 0.75rem;
}
.advanced-toggle:hover { color: #a0aec0; border-color: #4a5568; }
.advanced-content { display: none; }
.advanced-content.visible { display: block; }

/* Global toggle */
.global-toggle { text-align: right; margin-bottom: 1rem; font-size: 0.85rem; }
.global-toggle label { cursor: pointer; color: #718096; }
.global-toggle input { accent-color: #f6851b; margin-right: 0.4rem; }

/* Step navigation */
.steps-nav { display: flex; gap: 0.5rem; margin-bottom: 2rem; flex-wrap: wrap; }
.step-indicator {
  padding: 0.5rem 1rem; border-radius: 8px; background: #2d3748;
  color: #718096; font-size: 0.85rem; cursor: pointer; flex: 1; text-align: center;
}
.step-indicator.active { background: #f6851b; color: #fff; }
.step-indicator.completed { background: #1a4731; color: #68d391; }

/* Field rows */
.field-row { display: flex; gap: 1rem; }
.field-row .field { flex: 1; }

/* Raw TOML preview */
#rawToml {
  background: #0f1117; border: 1px solid #2d3748; border-radius: 8px;
  padding: 1rem; font-family: 'SF Mono', 'Fira Code', monospace;
  font-size: 0.82rem; line-height: 1.5; overflow-x: auto; white-space: pre;
  margin-top: 1rem;
}
```

- [ ] **Step 4: Build and verify**

Run: `cargo build`
Expected: compiles without errors

### Task 4: Update CLI wizard

**Files:**
- Modify: `src/bin/setup.rs` — CLI mode

- [ ] **Step 1: Add --advanced flag and new CLI prompts**

Add an `--advanced` flag that skips the "Configure advanced?" prompts and shows all settings upfront:

```rust
let advanced = std::env::args().any(|a| a == "--advanced");
```

After the basic prompts, add a conditional section:

```rust
// Advanced: Agent
let show_advanced = advanced || ask("Configure advanced settings?", "n");
if show_advanced {
    let max_iter = prompt("Max iterations", "25");
    let empty_retry = prompt("Empty response retry limit", "3");

    // Vision/OCR
    let supports_vision = ask("Supports vision?", "n");
    let base_url = prompt("OpenRouter base URL", "https://openrouter.ai/api/v1");
    let ocr_model_dir = prompt("OCR model directory", "~/.cache/ocrs");

    // LangSmith
    if ask("Configure LangSmith?", "n") {
        let ls_key = prompt_secret("LangSmith API key", "");
        let ls_project = prompt("LangSmith project", "rustfox");
    }

    // Embedding
    if ask("Configure embedding API?", "n") {
        let emb_key = prompt_secret("Embedding API key", "");
        let emb_url = prompt("Embedding base URL", "https://openrouter.ai/api/v1");
        let emb_model = prompt("Embedding model", "qwen/qwen3-embedding-8b");
        let emb_dim = prompt("Embedding dimensions", "1536");
    }

    // Learning
    let skill_extract = ask("Enable skill extraction?", "y");
    let skill_threshold = prompt("Extraction threshold (tool calls)", "5");

    // Query rewriting
    let qr = ask("Enable query rewriting?", "n");

    // Supervisor mode
    let sup_mode = prompt("Supervisor autonomy mode", "standard");
}
```

- [ ] **Step 2: Build and verify**

Run: `cargo build`
Expected: compiles without errors

### Task 5: Test and verify

- [ ] **Step 1: Run existing tests**

Run: `cargo test --bin setup`
Expected: all tests pass

- [ ] **Step 2: Manual smoke test**

Run: `cargo run --bin setup`
Expected: wizard opens in browser, all 4 steps render correctly, advanced sections collapse/expand, save produces valid config.toml

- [ ] **Step 3: Build release**

Run: `cargo build --release`
Expected: compiles without errors
