# Setup Wizard Redesign — Design

**Date:** 2026-06-11
**Status:** Draft

## Problem

The setup wizard (`setup/index.html` + `src/bin/setup.rs`) only handles ~10 config fields, while `config.example.toml` now has ~30+ settings across 12 sections. Major gaps:

| Missing from wizard | Config sections |
|---------------------|-----------------|
| Vision/OCR config | `openrouter.supports_vision`, `ocr.model_dir` |
| Agent loop | `agent.max_iterations`, `agent.empty_response_retry_limit` |
| Embedding API | `embedding.api_key`, `base_url`, `model`, `dimensions` |
| LangSmith | `langsmith.api_key`, `project` |
| Learning | `learning.user_model_path`, `skill_extraction_enabled`, etc. |
| Skills/Agents directory | `skills.directory`, `agents.directory` |
| Query rewriting | `memory.query_rewriter_enabled` |
| Home directory | `general.home` |
| Supervisor config | `supervisor.default_autonomy_mode`, `supervisor.risk.*` |
| OpenRouter base URL | `openrouter.base_url` |
| HTTP MCP servers | Already parsed but not configurable in wizard UI |

Non-tech users are intimidated by the raw config file. Tech users want full control.

## Goals

1. **Progressive disclosure** — non-tech users see only essentials with sensible defaults; tech users can expand to see every option
2. **Full config coverage** — every setting in `config.example.toml` is configurable through the wizard
3. **Smart defaults** — all fields have pre-filled defaults so you can click through without typing
4. **Existing config import** — loading an existing `config.toml` pre-fills all fields
5. **Both web + CLI** — same capabilities in both interfaces

## Non-Goals

- Changing the TOML config format or config.rs parsing
- Adding new config sections beyond what's in `config.example.toml`
- Removing the manual `config.toml` editing path

## Design

### Progressive Disclosure Pattern

```
┌─────────────────────────────────────────────────┐
│  ⚙️ [Show all settings]          Progress: ███░ │  ← Global toggle
├─────────────────────────────────────────────────┤
│                                                 │
│  Step 1: 🤖 Telegram Bot                        │
│  ┌─────────────────────────────────────────────┐│
│  │ Bot Token: [___________________________] ◄  ││  ← Required
│  │ User IDs:  [___________________________]    ││  ← Required
│  │ [▼ Advanced settings]                       ││  ← Collapsed by default
│  │   System prompt: [textarea...]              ││
│  │   Model: [moonshotai/kimi-k2.5 _________]   ││
│  │   Max tokens: [4096]                        ││
│  │   Supports vision: [□]                      ││
│  └─────────────────────────────────────────────┘│
│                                                 │
│  Step 2: 🌍 Location & Storage                  │
│  ┌─────────────────────────────────────────────┐│
│  │ Location: [Tokyo, Japan ________________]    ││  ← Shown by default
│  │ [▼ Advanced settings]                       ││
│  │   Sandbox dir: [~/.rustfox/workspace ____]  ││
│  │   DB path:     [~/.rustfox/rustfox.db ___]  ││
│  │   Skills dir:  [~/.rustfox/skills _______]  ││
│  │   Home dir:    [~/.rustfox _______________]  ││
│  │   OCR model dir: [~/.cache/ocrs _________]  ││
│  └─────────────────────────────────────────────┘│
│                                                 │
│  Step 3: 🧩 Integrations                        │
│  ┌─────────────────────────────────────────────┐│
│  │ ☑ Git  ☐ Filesystem  ☑ Google Workspace    ││  ← Visual cards
│  │ ☐ Threads  ☐ Brave Search  ☐ Notion  ☐ Exa ││
│  │ [▼ Advanced settings]                       ││
│  │   LangSmith key:  [_____________________]   ││
│  │   LangSmith proj: [rustfox ______________]  ││
│  │   Embedding key:  [_____________________]   ││
│  │   Embedding model: [qwen/qwen3-embedding-8b]││
│  │   Learning extraction: [☑]                  ││
│  └─────────────────────────────────────────────┘│
│                                                 │
│  Step 4: ✅ Review & Save                       │
│  ┌─────────────────────────────────────────────┐│
│  │ Summary of all settings                     ││
│  │ [📋 Show raw TOML]                         ││
│  │ [Save config]                               ││
│  └─────────────────────────────────────────────┘│
└─────────────────────────────────────────────────┘
```

### Web UI Design Principles

1. **Single-page wizard** with step indicator and back/next navigation
2. **Dark theme** matching the current RustFox brand
3. **Inline validation** — red border + error message on invalid fields
4. **Placeholder defaults** — all fields show their default value as placeholder text
5. **Progress saved** — navigating between steps doesn't reset form state
6. **Responsive** — works on mobile for field setup from phone
7. **"Show all settings" toggle** at the top that opens every advanced section

### CLI TUI Design

Same progressive disclosure but adapted for terminal:

```
$ cargo run --bin setup -- --cli

RustFox Setup Wizard
====================
Press Enter to accept [defaults].

Step 1/4: Telegram Bot
  Bot token [required]: _________________________
  User IDs (comma-separated) [required]: ________
  Configure advanced settings? [y/N]: y
    System prompt [default provided]:
    Model [moonshotai/kimi-k2.5]:
    Max tokens [4096]:
    Supports vision [false]:

Step 2/4: Location & Storage
  Location [optional]: Tokyo, Japan
  Configure advanced settings? [y/N]: n

...

Config saved to /home/user/.rustfox/config.toml
```

The `--advanced` flag skips the "Configure advanced settings?" prompts and shows everything upfront.

### Rust Backend Changes (`src/bin/setup.rs`)

New raw parse structs needed:

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
    supervisor: Option<RawSupervisor>,
    subagents: Option<RawSubagents>,
    skills: Option<RawSkills>,
    agents_config: Option<RawAgentsConfig>,
    #[serde(default)]
    mcp_servers: Vec<RawMcpServer>,
}

#[derive(Deserialize, Default)]
struct RawOpenRouter {
    api_key: Option<String>,
    model: Option<String>,
    base_url: Option<String>,
    max_tokens: Option<u32>,
    system_prompt: Option<String>,
    supports_vision: Option<bool>,
}

#[derive(Deserialize, Default)]
struct RawMemory {
    database_path: Option<String>,
    query_rewriter_enabled: Option<bool>,
}

#[derive(Deserialize, Default)]
struct RawAgent {
    max_iterations: Option<u32>,
    empty_response_retry_limit: Option<u32>,
}

#[derive(Deserialize, Default)]
struct RawSupervisor {
    default_autonomy_mode: Option<String>,
    require_approval_for_low: Option<bool>,
    require_approval_for_medium: Option<bool>,
    auto_execute_only_low: Option<bool>,
}

#[derive(Deserialize, Default)]
struct RawSkills {
    directory: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawAgentsConfig {
    directory: Option<String>,
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
struct RawSubagents {
    default_tools: Option<Vec<String>>,
}
```

With corresponding `ExistingConfig` response fields for each. The `save_config` endpoint must serialize all sections, not just the current subset.

### HTML/CSS Changes

- Add new form sections in `setup/index.html` with `class="advanced"` for collapsible groups
- CSS animations for expand/collapse transitions
- Toggle for "Show all settings" that sets `localStorage` preference
- Raw TOML preview in Step 4 using a `<pre>` block with syntax highlighting

## Implementation Plan

### Step 1: Add new parse structs to setup.rs
- Add `RawAgent`, `RawLangSmith`, `RawEmbedding`, `RawOcr`, `RawLearning`, `RawSupervisor`, `RawSubagents`, `RawSkills`
- Update `ExistingConfig` with all new fields
- Update `load_config()` to populate them

### Step 2: Update save_config to serialize all sections
- Current `save_config` only writes known fields
- Expand to write ALL config sections correctly formatted

### Step 3: Rewrite setup/index.html with progressive disclosure
- New step layout with sidebar progress
- Collapsible "Advanced" sections per step
- "Show all settings" global toggle
- Raw TOML preview in final step
- Inline validation for all fields

### Step 4: Update CLI wizard
- Same progressive disclosure pattern for terminal
- `--advanced` flag for full control

## Deferred

- Internationalization (multi-language setup)
- MCP server health check after save
- Config validation beyond basic field presence
