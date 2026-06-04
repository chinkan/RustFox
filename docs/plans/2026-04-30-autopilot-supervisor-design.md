# RustFox Autopilot Supervisor — Design (Spec v2)

> Source: user-provided spec, lightly reformatted for the repo. This is the design
> document that the implementation plan (`2026-04-30-autopilot-supervisor.md`)
> derives from.

## 1. Purpose

RustFox shall evolve from a task-oriented AI assistant into a general-purpose
autonomous **task supervisor** for daily use. It must be able to:

- accept user intent in natural language,
- classify the task,
- decide the safest and most appropriate execution path,
- choose one or more execution backends,
- orchestrate multi-step work end to end,
- verify results,
- preserve an audit trail,
- hand control back to the user when needed.

This version is **backend-agnostic**. It must support Claude Code CLI, Codex CLI,
other AI CLIs, shell jobs, MCP tools, and local scripts as interchangeable
execution targets.

## 2. Design Goals

- **Generality** — coding, research, writing, admin, automation, ops, file
  transformation, workflow, and general assistant tasks.
- **Autonomy** — complete low-risk tasks without constant user intervention.
- **Safety** — never perform risky actions without explicit policy authorization
  or human approval.
- **Determinism** — every run replayable from stored artifacts, logs, state, and
  outputs.
- **Extensibility** — new backends, skills, policies, and task types addable
  without modifying core supervisor logic.

## 3. Non-Goals

- Depend on a single CLI vendor.
- Hardcode Claude Code into the core architecture.
- Force design/spec/plan steps for every task.
- Require git worktrees for non-code tasks.
- Ask for approval for every low-risk operation.
- Merge or deploy without policy permission.

## 4. Core Principles

1. **Task-first, not tool-first** — reason about the task first, then choose tools.
2. **Capability-based backend selection** — backends chosen by capability
   (reasoning, shell execution, code editing, review, research, document
   creation, long-running job control).
3. **Risk-based autonomy** — lower the risk, more the system may execute
   automatically.
4. **Evidence-based completion** — task is not done until required evidence
   exists.
5. **Resume over restart** — all state persistable and resumable.

## 5. System Overview

Five major layers:

1. **Intake Layer** — Telegram, CLI, API, webhook, future UI.
2. **Task Intelligence Layer** — classification, intent inference, constraint
   detection, workflow selection.
3. **Policy Layer** — auto-execute vs ask vs escalate; backend choice;
   clarification gating.
4. **Execution Layer** — runs jobs through one or more backends.
5. **Verification & Archive Layer** — checks outputs, stores artifacts, records
   final result.

## 6. Core Abstractions

### 6.1 Task

Normalized unit of user intent.

Fields: `task_id, title, user_request, task_type, priority, risk_level,
required_capabilities, constraints, inputs, expected_outputs, approval_policy,
execution_mode, status, artifacts, current_stage`.

Task types: `code_change, bug_fix, refactor, research, writing, ops_automation,
workflow_automation, data_transformation, decision_support, general_assistant,
unknown`.

### 6.2 Job

Executable unit assigned to a backend.

Fields: `job_id, task_id, job_type, backend_type, goal, prompt, input_context,
timeout, retry_policy, allow_tools, workspace, expected_artifacts, status,
result, logs`.

Job types: `planner_job, executor_job, reviewer_job, verifier_job, research_job,
shell_job, document_job, approval_job`.

### 6.3 Backend

Any executor that can complete a job. Examples: Claude Code CLI, Codex CLI,
local LLM CLI, shell subprocess, MCP tool bridge, script runner, browser
automation, document generator, test runner.

Each backend declares: `name, version, capabilities, supported_job_types,
input_contract, output_contract, timeout_behavior, retry_behavior,
failure_modes, security_constraints`.

### 6.4 Skill

A reusable workflow package — procedural knowledge and execution instructions
(not a backend). Examples: brainstorming, planning, writing specs, executing
code changes, reviewing changes, verifying results, closing tasks, handling
clarification, selecting tools, managing worktrees.

### 6.5 Policy

Decision framework for: choosing an execution path, answering questions,
determining approval requirements, permitting or denying actions, escalating to
the user.

## 7. Task Lifecycle

`Intake → Classify → Route → Clarify (if needed) → Plan → Execute → Verify →
Report → Archive`.

## 8. Workflow Modes

- **Fast Mode** — low-risk, low-complexity (intake → classify → execute →
  verify → report). Examples: summarize a file, run a simple command.
- **Standard Mode** — ordinary multi-step tasks (adds clarify, plan, archive).
- **Rigorous Mode** — high-risk or code-heavy (adds brainstorm, design, spec,
  review).

## 9. Supervisor Architecture

Components:

- **Intake Router** — accept input, extract intent, detect ambiguity, infer
  task type, normalize task object.
- **Task Classifier** — category, complexity, risk, branch/worktree need,
  approval gate need.
- **Policy Engine** — clarification answers, defaults, auto-execute vs
  escalate, single vs multi-backend.
- **Planner** — task plan, jobs, dependencies, verification & completion
  criteria.
- **Backend Selector** — capability-based selection with fallback and
  multi-backend pipelines.
- **Execution Orchestrator** — submits jobs, tracks status, captures logs,
  retries/aborts, manages subjobs and long-running work.
- **Verification Engine** — checks outputs, runs tests/validations, prevents
  false completion.
- **Artifact Manager** — persists plans, prompts, responses, logs, transcripts,
  outputs, final summaries.

## 10. Backend-Agnostic Adapter Interface

Required: `capabilities(), can_handle(job_type), prepare(job), run(job),
collect_result(), verify_result(), cancel(), resume()`.

Optional: `stream_output(), spawn_subjob(), use_workspace(), use_tools(),
request_approval()`.

Output contract: every backend produces `status, summary, evidence, errors,
changed_files (if applicable), next_step_recommendation`.

## 11. Policy Decision Model

Deterministic rules.

- **Inputs**: task type, risk level, backend capability, workspace state, user
  preferences, repository preferences, tool permissions, confidence score.
- **Outputs**: continue automatically, ask user, choose option, use fallback
  backend, split task, require approval, stop and report.

Example rules:

- Low-risk + well-scoped → auto-execute.
- Affects external systems → require approval.
- Code-related + repo requires isolation → use a worktree.
- Backend lacks needed capability → reroute.
- High ambiguity → clarify.

## 12. Workflow Templates

- **Coding**: classify → brainstorm → design → spec → plan → branch/worktree (if
  needed) → implement → review → verify → finish.
- **Research**: classify → gather sources → compare alternatives → summarize →
  recommend → archive.
- **Writing**: classify → outline → draft → revise → polish → verify → report.
- **Ops**: classify → inspect environment → run plan → execute → verify →
  report → archive.
- **General assistant**: classify → answer-only or action → execute/respond →
  log.

## 13. Branch and Workspace Management

Optional and task-dependent.

- **Required for**: code changes, tests, repo refactors, patch generation,
  reviewable engineering work.
- **Responsibilities**: create or reuse branch, isolated workspace, store
  workspace mapping, prevent collisions, cleanup on finish/failure.
- **Not required for**: pure Q&A, summarization, research, document generation,
  scheduling, general assistant tasks.

## 14. Artifact Model

Every task generates artifacts appropriate to its type.

- **Common**: intake record, classification, policy decisions, job plan,
  execution log, result summary, error summary, final archive record.
- **Code-task**: brainstorm.md, design.md, spec.md, plan.md, review.md,
  verification.md, finish.md.
- **Research-task**: sources.md, comparison.md, conclusion.md.
- **Writing-task**: outline.md, draft.md, revision.md.

## 15. Skills Architecture

Grouped by workflow family.

- **Core**: task intake, classification, clarification, policy resolution,
  planning, execution orchestration, review, verification, completion, cleanup.
- **Code-focused**: brainstorming, design, spec writing, implementation
  execution, code review, branch finishing.
- **General-purpose**: research, summarization, file processing, command
  orchestration, document generation, report generation.

Each skill defines: `purpose, when to use, inputs, outputs, operating rules,
stop conditions`.

## 16. Execution Strategy

- **Single backend** — one backend for the whole job.
- **Staged backend** — different backends per stage (planner → executor →
  reviewer → verifier).
- **Parallel workers** — multiple jobs in parallel when safe.
- **Fallback execution** — if preferred backend fails, try fallback.

## 17. Verification Requirements

A task is complete only when required evidence exists.

Evidence examples: exit code success, tests passed, files created, diff
reviewed, output file validated, user-visible result confirmed, logs stored.

Rules: no completion without evidence, no success without artifact storage, no
silent failure, no skipped checks for rigorous tasks.

## 18. Safety and Guardrails

Must respect: command whitelists, workspace boundaries, file access
restrictions, network restrictions, secret redaction, external side-effect
approval.

High-risk actions (always stricter control): deletion, destructive shell
commands, remote deployment, credential use, account actions, money-related
actions, external API writes, production changes.

When denied: explain reason, offer safer alternative, preserve current state.

## 19. Observability

Logs: user request, classification result, policy decisions, backend selection,
job prompts, job outputs, errors, retries, verification results, final summary.

Metrics: task duration, stage duration, retries, clarifications, approval rate,
failure rate, auto-completion rate.

Traceability: every task traceable by `task_id, job_id, backend_id,
workspace_id, artifact_ids`.

## 20. Configuration

- **Global**: default autonomy mode, risk thresholds, timeout defaults, retry
  defaults, backend preferences, logging level, artifact retention policy.
- **Per-repo**: repo path, default branch, build/test commands, format/lint
  commands, workspace root, file restrictions, preferred skills, preferred
  backends.
- **Per-task**: task type, urgency, approval requirements, execution mode,
  backend preference, time budget.

## 21. Backend Categories

- **Reasoning** — planning, clarification, decision support, structured
  thinking.
- **Coding** — code edits, refactors, patch generation, repository operations.
- **Shell** — command execution, file operations, system tasks, scripted
  automation.
- **Research** — web research, source comparison, fact gathering.
- **Document** — markdown / DOCX / PDF / spreadsheet generation, report
  assembly.
- **MCP** — tool-based integrations, external systems, structured context
  access.

## 22. Recommended Default Modes

- **Daily use** — Standard mode with low-friction auto-execution for safe
  tasks.
- **Code work** — Rigorous mode with branch/worktree and review.
- **Research** — Standard mode with source gathering and summary.
- **Ops** — Strict policy with explicit approval for side effects.

## 23. State Machine

States: `INTAKE, CLASSIFY, ROUTE, CLARIFY, PLAN, PREPARE_WORKSPACE, EXECUTE,
REVIEW, VERIFY, REPORT, ARCHIVE, PAUSED, FAILED, CANCELLED, DONE`.

Rules: explicit transitions; invalid transitions fail; state persisted after
each transition; resume continues from last stable state.

## 24. Implementation Milestones

- **M1** — General task intake, classification, policy, artifact storage.
- **M2** — Backend abstraction + first executor backend.
- **M3** — Plan/execute/verify/report loop for general tasks.
- **M4** — Branch/worktree integration for code tasks.
- **M5** — Skill packs for multiple workflows.
- **M6** — Parallel jobs, fallback backends, subjob orchestration.
- **M7** — Fully autonomous daily assistant mode with risk-based gating.

## 25. Definition of Done

RustFox v2 is complete when it can:

- accept arbitrary user tasks,
- classify them correctly,
- choose an execution workflow,
- select the best backend,
- answer clarifying questions by policy,
- execute jobs safely,
- verify outcomes,
- manage code workspaces when needed,
- manage non-code jobs when needed,
- persist all important artifacts,
- resume interrupted work,
- report completion clearly.

## 26. Final Design Statement

RustFox should be a general autonomous task supervisor with a task router, a
policy engine, pluggable backends, reusable skills, explicit workflows,
evidence-based completion, and resumable state. Claude Code CLI, Codex CLI,
shell jobs, MCP jobs, and future tools should be treated as **interchangeable
execution backends**, not as architectural assumptions.

---

## Mapping to Existing RustFox Code

The plan that derives from this spec must not greenfield — it must integrate
with the existing module layout:

| Spec concept | Existing module / extension point |
|---|---|
| Intake Layer | `src/platform/` (`telegram.rs` exists; CLI/HTTP added later) |
| Agentic loop | `src/agent.rs::Agent::process_message` (kept; supervisor wraps it) |
| Skills | `src/skills/` + `skills/` directory + `loader.rs` |
| Tools / MCP | `src/tools.rs` + `src/mcp.rs` |
| Persistence | `src/memory/` (SQLite + FTS5 + sqlite-vec); add new tables |
| Background jobs | `src/scheduler/` (`tokio-cron-scheduler`, `ScheduledTaskStore`) |
| Configuration | `src/config.rs` (TOML) — extend with `[supervisor]` section |
| Observability | `tracing` + `langsmith.rs` |

New top-level supervisor module to be added as `src/supervisor/` with submodules
for `task`, `job`, `policy`, `state`, `backend` (adapter trait + registry),
`workflow`, `verification`, `artifact`, and `orchestrator`. Concrete backends
live under `src/supervisor/backends/{shell,llm,mcp,claude_code_cli,codex_cli,
script}.rs`. The existing `Agent` becomes the default *reasoning backend*
implementing the new adapter trait.
