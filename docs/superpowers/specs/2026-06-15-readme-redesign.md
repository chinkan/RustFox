# README Redesign — Split into README.md + docs/ Reference Files

## Summary

The current `README.md` (449 lines) is a single-file wall of content that tries
to be everything at once — landing page, full config reference, MCP server
guide, architecture docs, roadmap, and dependency list. This spec proposes
splitting it into three focused files following the best-practice patterns
from top open-source projects (LobeHub, zoxide, Rust, Fiber, etc.).

## Motivation

- **First-impression fatigue:** All 35 features listed in one bullet block —
  readers can't find why they care in the first 10 seconds.
- **SEO/AEO dilution:** The first 200 characters contain generic text
  ("logo", "badge", "CI") instead of the keyword-dense value proposition.
- **Content density:** MCP server config (60 lines), architecture tree
  (40 lines), roadmap (40 lines), and dependency list (15 lines) all compete
  for attention with the quick-start path.
- **Maintenance tax:** Every new feature, tool, or config option adds to the
  same ever-growing file.

## Design

### File Structure

```
README.md          (~150 lines) — Landing page / first impression
docs/GUIDE.md      (~300 lines) — Full reference: config, tools, commands, advanced
docs/ARCHITECTURE.md (~120 lines) — Source tree, data flow, component descriptions
```

### README.md — Landing Page

```
Section                     Content
────────────────────────────────────────────────────────────────────
[Hero]                      Logo (centered), title, subtitle, badges
                            (CI, License, Buy Coffee, GitHub Sponsors)
[Value Proposition]         "A self-hosted, agentic Telegram AI assistant
                             written in Rust, powered by OpenRouter LLM
                             with sandboxed tools, MCP integration, and
                             persistent memory."
                            (SEO: first 200 chars contain Telegram AI
                             assistant, Rust, agentic, OpenRouter, MCP,
                             sandboxed, LLM, self-hosted)

[Features]                  8 hero features selected to highlight
                            RustFox's unique value (moved from current
                            35-bullet list). The remaining features
                            (streaming, file/image OCR, RAG query
                            rewriting, long-term memory, nightly
                            summarization, post-task learning,
                            supervisor, LangSmith tracing, instance +
                            bundled layering, etc.) go to GUIDE.md
                            Advanced Features section.

                            🤖 AI Agent — OpenRouter, agentic loop, tools
                            🔧 Built-in Tools — file I/O, exec, scheduling
                            🧩 MCP Servers — plug any MCP server
                            🧠 Persistent Memory — SQLite + vector RAG
                            🧬 Skills & Agents — folder-based skills
                            🤝 Agent Layer — isolated subagents with tool
                                              whitelist
                            🔄 Task Scheduling — cron/one-shot
                            📦 Self-Hosting — single binary, wizard, service

[Quick Start]               4 steps with copy-paste code blocks:
                            1. Install (release download or cargo install)
                            2. Configure (rustfox --setup)
                            3. Run (rustfox)
                            4. (Optional) Install as background service
                               (rustfox --service install)

[Configuration]             Key settings table (6 essentials):
                            bot_token, allowed_user_ids, api_key, model,
                            sandbox, mcp_servers
                            → Full reference: docs/GUIDE.md

[Quick Tool Overview]       Brief one-liner table (6 core tools):
                            read_file, write_file, send_file,
                            execute_command, schedule_task, invoke_agent
                            → Full tool reference: docs/GUIDE.md

[Architecture]              ~3 lines + link to docs/ARCHITECTURE.md

[docs links footer]         README.md │ GUIDE.md │ ARCHITECTURE.md

[Contributing + License]    Same as current

[Support]                   Buy Coffee + GitHub Sponsors badges
```

### docs/GUIDE.md — Full Reference

```
Section                     Content
────────────────────────────────────────────────────────────────────
[H1 + TOC]                  RustFox Guide with collapsible TOC

[Configuration]             All TOML settings in one table:
                            telegram, openrouter, sandbox, memory,
                            embedding, ocr, skills, agents, subagents,
                            mcp_servers, general, agent, langsmith,
                            learning, supervisor
                            Each row: Setting, Description, Default

[MCP Server Integration]    Full guide:
                            - Prerequisites (uvx, npx)
                            - Config syntax (stdio, HTTP, OAuth)
                            - Popular MCP servers table (Git, Brave
                              Search, GitHub, Fetch, Filesystem,
                              SQLite, Puppeteer, Threads)
                            - Complete config examples
                            - Tool naming convention

[Built-in Tools]            All category tables:
                            Core, Scheduling, Memory (remember,
                            recall, search_memory), Skills, Agents,
                            Plan, Utility

[Bot Commands]              All wired commands table (currently 6:
                            /start, /clear, /tools, /skills, /verbose,
                            /query-rewrite). Supervisor commands
                            (/supervise, /tasks, /resume, /cancel,
                            /approve, /clarify) are planned — mark as
                            "coming soon" or omit until telegram dispatch
                            is wired in M7.3

[Skills & Agents System]    How skills load, subagent vs instruction,
                            agent layer, update engine, verifier

[Advanced Features]         File processing, RAG, summarization,
                            post-task learning, supervisor, LangSmith

[Roadmap]                   Done + Planned (same as current)

[Dependencies]              Key crate table (same as current)
```

### docs/ARCHITECTURE.md

```
Section                     Content
────────────────────────────────────────────────────────────────────
[H1 + TOC]                  Architecture

[Source Tree]               Full src/ tree with annotations
                            (moved from current README)

[Data Flow]                 ASCII diagram showing message lifecycle:
                            User → Telegram → Agent → LLM
                              ↓ tool call
                            execute_tool() → built-in | MCP | skills
                              ↓ result
                            back to LLM → final response → Telegram

[Key Components]            Brief descriptions:
                            Agent, LlmClient, McpManager,
                            SkillRegistry, Memory, Scheduler,
                            Supervisor, FileProcessor

[Agentic Loop]              4-step explanation of the loop
```

## SEO/AEO Strategy

- **First 200 characters** contain: "Telegram AI assistant", "Rust",
  "agentic", "OpenRouter", "MCP", "sandboxed", "LLM"
- **H1** contains: "RustFox — Telegram AI Assistant" (keyword-rich)
- **Feature section** uses natural language an AI answer engine would
  surface: "a self-hosted agentic Telegram AI assistant written in Rust"
- **Quick Start** answers common queries: "how to install RustFox",
  "how to configure Telegram bot", "how to run RustFox as a service"
- **All external links** use descriptive anchor text (not "click here")

## Implementation Notes

- **`learning.user_model_path`** — The config key lives under `[learning]`, not `[memory]`.
  GUIDE.md must use the correct TOML path.
- **`patch_skill`** — Listed in both Skill Tools and Utility Tools in current
  README. Keep only in Skill Tools (it operates on skill files).
- **Max iterations default** — Config default is 25, not 10. README must say 25.
- **Wired commands only** — Only `/start`, `/clear`, `/tools`, `/skills`,
  `/verbose`, `/query-rewrite` are wired in the Telegram dispatcher
  (`telegram.rs`). `/update-skills` is wired but only documented in README.
  Supervisor commands are not yet dispatched — GUIDE.md must note this.

## Content Removed from README.md

- Full MCP server configuration guide → moved to docs/GUIDE.md
- Exhaustive settings table (was 20+ rows) → trimmed to 6 key settings;
  full table → docs/GUIDE.md
- Architecture source tree (was 40 lines) → line-count link to docs/ARCHITECTURE.md
- Full tool tables (was 6 tables) → table of 6 core tools; full tables → docs/GUIDE.md
- Full bot commands table → docs/GUIDE.md
- Roadmap done/planned → docs/GUIDE.md
- Dependencies list → docs/GUIDE.md
- "Thanks" footnote → docs/GUIDE.md

## Content Preserved in README.md

- Logo + badges ✓
- One-liner value proposition ✓ (rewritten for SEO)
- Feature list ✓ (rewritten, concise, emoji-driven)
- Quick Start ✓ (unchanged structure, tightened)
- Key configuration ✓ (trimmed to 6 essentials)
- Tool overview ✓ (trimmed to 6 core, linked to full ref)
- Architecture ✓ (3-line summary + link)
- Contributing + License ✓
- Support badges ✓

## Out of Scope

- Rewriting or restructuring docs/ that already exist
- Changing `docs/` directory structure (new files only)
- Adding a documentation site or wiki
