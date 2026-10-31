# Architecture

## Source Tree

```
src/
├── main.rs           # Entry point, config loading, MCP setup, bot launch
├── config.rs         # TOML config parsing (all sections)
├── home.rs           # Persistent home directory resolution (~/.rustfox)
├── agent.rs          # Agentic loop, tool dispatch, skills/agents layer
├── agent_prompt.rs   # Prompt preparation, compaction, recovery nudges
├── tools.rs          # Built-in tool definitions + sandbox path validation
├── llm.rs            # OpenRouter API client with tool calling
├── mcp.rs            # MCP client manager for external tool servers
├── file_processor/   # File/attachment processing (OCR, vision, PDF, DOCX)
├── memory/           # SQLite persistence, vector embeddings, RAG, summarizer
├── scheduler/        # Cron/one-shot task scheduler with DB persistence
├── secret_store/     # SecretStore + pending + SecretBridge (env inject / redact)
├── skills/           # Skill loader, registry, embed/seeding, update engine
├── learning.rs       # Post-task skill extraction, user model persistence
├── langsmith.rs      # Optional LangSmith observability client
├── supervisor/       # Autopilot v2 — autonomous task runner
│   ├── mod.rs        # Facade (submit, execute_now, pause, resume, state)
│   ├── task.rs       # Task, TaskType, RiskLevel enums
│   ├── job.rs        # Job, JobType, JobStatus enums
│   ├── state.rs      # Transition-allowed state machine
│   ├── store.rs      # CRUD over sup_tasks / sup_jobs / sup_transitions
│   ├── intake.rs     # Raw text → Task normalization
│   ├── classifier.rs # Heuristic / LLM-backed / Skill-aware classifiers
│   ├── policy.rs     # PolicyEngine — auto-execute, clarify, approve gates
│   ├── planner.rs    # Task → Plan with parallel job groups
│   ├── workflow.rs   # Fast / Standard / Rigorous workflow templates
│   ├── orchestrator.rs  # Plan executor with fallback + parallel + subjobs
│   ├── verification.rs  # Evidence-gated verification engine
│   ├── artifact.rs   # ArtifactManager with secret redaction
│   ├── workspace.rs  # Per-task git worktree management
│   ├── reporter.rs   # Human-readable job summary
│   ├── redact.rs     # Secret scrubber for api_key / password / token
│   └── backend/      # Backends (reasoning, shell, MCP, claude-code, codex, script)
├── platform/         # Telegram bot handler + tool notifier
├── setup/            # Setup wizard (web + CLI) + service management
└── utils/            # String utilities, markdown-to-entities conversion

skills/               # Bundled skills (15+): code-interpreter, problem-solver,
│                     #   soul, news-fetcher, sup-* workflow packs, etc.
agents/               # Agent definitions (AGENT.md per agent)
└── verifier/         # Zero-trust verifier (read-only sandbox)
setup/                # Setup wizard HTML
```

## Data Flow

```
User ──Telegram──▶ bot.rs ──▶ Agent.process_message()
                                   │
                                   ▼
                            LlmClient.chat()
                            (OpenRouter API)
                                   │
                          ┌────────┴────────┐
                          │                 │
                     Tool call           Text reply
                          │                 │
                          ▼                 ▼
                    execute_tool()     Telegram send
                          │
              ┌───────────┼───────────┐
              ▼           ▼           ▼
        Built-in      MCP tool   Skills/Agents
        (tools.rs)   (mcp.rs)    (agent.rs)
              │           │           │
              └───────────┴───────────┘
                          │
                          ▼
              Result appended to history
                          │
                          ▼
                   Loop back to LLM
                   (up to max_iterations)
```

## Key Components

| Component | File | Role |
|-----------|------|------|
| **Agent** | `agent.rs` | Orchestrates the agentic loop: calls LLM, dispatches tools, manages conversation state |
| **LlmClient** | `llm.rs` | Stateless HTTP client for OpenRouter `/chat/completions` with tool-calling support |
| **McpManager** | `mcp.rs` | Manages stdio-based MCP child processes; tools namespaced `mcp_{server}_{tool}` |
| **SkillRegistry** | `skills/mod.rs` | Loads and manages skills/agents from the home directory with compile-time embedded fallback |
| **Memory** | `memory/` | SQLite-backed persistence, vector embeddings, hybrid search (FTS5 + vector), query rewriting, summarization |
| **Scheduler** | `scheduler/` | Cron and one-shot task scheduler with DB persistence; supports add/remove/list at runtime |
| **Supervisor** | `supervisor/` | Generic autonomous task runner: intake → classify → plan → execute → verify → report |
| **FileProcessor** | `file_processor/` | Handles image OCR, vision API calls, PDF/DOCX text extraction |
| **TurnGate** | `turn_gate.rs` | Per `session_key` (`{bot_id}:{user_id}`): interactive lane and scheduled lane (ADR-0020, ADR-0052) |

## Session turns

`TurnGate` allows one scheduled job and one interactive chat on the same bot+user at the same time.

- **Scheduled lane.** The job runner calls `begin_scheduled` before the agent loop and `finish_scheduled` after. The job waits to start until the interactive lane is idle. Inbound Telegram is never a steer into this loop (`drain_for(Scheduled)` is empty; the loop has no steer source).
- **Interactive lane.** A message while a schedule is running starts this lane immediately (`Submit::Start`). Further messages steer or queue only while this lane is busy (`Submit::Accepted`, `/mode steer` or `/mode queue`, 👀 ack). `finish_user` does not clear a schedule that is still running.
- **Cancel.** Interactive and scheduled loops use different cancel tokens (`{session_key}` and `{session_key}:sched`). A schedule timeout cancels the scheduled token and tools registered in that lane's scope. `/stop` cancels both lanes.

## Agentic Loop

The core loop in `Agent::process_message()` (`agent.rs`):

1. **Prepare** — Inject system prompt with skill/agent context, conversation history, and relevant RAG results
2. **Call LLM** — Send to OpenRouter with available tool definitions
3. **Check response type**:
   - **Tool call(s)** → Execute each tool via `execute_tool()`, append results to conversation, check max iterations, goto step 2
   - **Text response** → Send to user via Telegram, update conversation state, run post-task learning
4. **Error recovery** — If LLM returns an error or malformed response, append recovery nudge and retry (up to max iterations)
