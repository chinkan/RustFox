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
| **Scheduler** | `scheduler/` | Cron and one-shot task scheduler with DB persistence; supports add/remove/list at runtime. A run is `completed` only after [`scheduler/completion.rs`](../src/scheduler/completion.rs) accepts it ([ADR-0052](adr/0052-scheduler-completion-not-model-speech.md)) |
| **Supervisor** | `supervisor/` | Generic autonomous task runner: intake → classify → plan → execute → verify → report |
| **FileProcessor** | `file_processor/` | Handles image OCR, vision API calls, PDF/DOCX text extraction |

## Scheduled run completion

`run_one_scheduled_job` (`src/main.rs`) used to mark every non-max-iterations final reply `completed`. [ADR-0052](adr/0052-scheduler-completion-not-model-speech.md) inserts a verdict first:

1. If the task's `expected_artifact` glob matches a regular file with `mtime >= run_started_at - lookback` (default lookback 900s, never tighter than 300s), the run is `completed`. A guard rewrite or a short clarify on that same run is only a warning.
2. Otherwise a guard rewrite (`RunOutcome.rewritten_by_guard`: cold-intro, lost-context, or meta-token filler) is `failed`.
3. Otherwise a short reply (&lt; 200 characters) that matches a clarify/menu phrase is `failed`. Length alone does not fail a run. The phrase list has false positives on legitimately short tasks; set a glob when the task is supposed to be short.
4. `expected_artifact` NULL keeps the previous path for ordinary replies.

Failure uses the manual dead-letter gate (`enqueue_manual` / `mark_awaiting_user`, `HumanGateKind::UnverifiedResult`), not the transient auto re-fire queue. Interactive chat still shows the rewritten text and does not consult this verdict. Threads API verification is not part of this check.

## Agentic Loop

The core loop in `Agent::process_message()` (`agent.rs`):

1. **Prepare** — Inject system prompt with skill/agent context, conversation history, and relevant RAG results
2. **Call LLM** — Send to OpenRouter with available tool definitions
3. **Check response type**:
   - **Tool call(s)** → Execute each tool via `execute_tool()`, append results to conversation, check max iterations, goto step 2
   - **Text response** → Send to user via Telegram, update conversation state, run post-task learning
4. **Error recovery** — If LLM returns an error or malformed response, append recovery nudge and retry (up to max iterations)
