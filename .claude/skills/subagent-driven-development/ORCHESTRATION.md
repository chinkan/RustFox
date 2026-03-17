# Subagent orchestration reference

Reference for the **subagent-driven-development** skill. Load this file when you need routing rules, invocation quality, or the distinction between skills and subagents. The main workflow is in [SKILL.md](./SKILL.md); this document is supporting material (Level 3 resource per [Agent Skills](https://docs.anthropic.com/en/docs/agents-and-tools/agent-skills/overview)).

---

## Skills vs subagents

| Use **skills** when… | Use **subagents** when… |
|----------------------|-------------------------|
| Single-purpose, one-shot task (changelog, format) | Long research or exploration (context isolation) |
| Quick, repeatable action | Multiple workstreams in parallel |
| Task completes in one shot | Task needs specialized expertise across many steps |
| No separate context window needed | You want independent verification of work |

Skills load on-demand (metadata at startup, instructions when triggered). Subagents get a fresh context window and return a compressed result to the parent.

In this skill, the workflow uses subagents (implementer → spec-reviewer → code-quality-reviewer) per task. Agent definitions live in `../../agents/` (e.g. `implementer.md`, `spec-reviewer.md`, `code-quality-reviewer.md`). Dispatch templates are in this directory: [implementer-prompt.md](./implementer-prompt.md), [spec-reviewer-prompt.md](./spec-reviewer-prompt.md), [code-quality-reviewer-prompt.md](./code-quality-reviewer-prompt.md).

---

## Routing: parallel vs sequential vs background

**Parallel dispatch** (all must be true):

- 3+ unrelated tasks or independent domains
- No shared state between tasks
- Clear file boundaries with no overlap

**Sequential dispatch** (any one triggers):

- Tasks have dependencies (B needs output from A)
- Shared files or state (merge conflict risk)
- Unclear scope (need to understand before proceeding)

**Background dispatch:**

- Research or analysis (no file modifications)
- Results are not blocking current work

**For this skill:** Execute tasks **sequentially** (one implementer at a time), then spec review, then code quality review. Do not run multiple implementers in parallel on the same codebase.

---

## Invocation quality (context density)

Subagent failures are often **invocation failures**: vague instructions, thin context, or unclear deliverables. The parent must send **complete invocations**.

**Bad:** "Fix authentication"  
**Good:** "Fix OAuth redirect loop where successful login redirects to /login instead of /dashboard. Reference the auth middleware in src/lib/auth.ts."

Every subagent dispatch must include:

1. **Full task text** — Paste the task from the plan; do not make the subagent read a file.
2. **Scene-setting context** — Where this task fits, dependencies, architecture.
3. **Relevant references** — File paths, SHAs, or snippets the subagent needs.
4. **Clear success criteria** — What “done” looks like and how to report back.

---

## Four components of a good dispatch

1. **Delegation** — One clear task per subagent; single responsibility.
2. **Isolated context** — Subagent has no prior conversation; give everything in the prompt.
3. **Task processing** — Subagent uses its own instructions and tools; parent stays in orchestration role.
4. **Result compression** — Subagent returns a concise summary (what was done, what was verified, any issues).

---

## Project subagents

| Agent | Purpose | When to invoke |
|-------|---------|----------------|
| **implementer** | Implements one plan task; tests, commits, self-reviews | Per task (use [implementer-prompt.md](./implementer-prompt.md) or invoke by name) |
| **spec-reviewer** | Checks implementation matches spec (by reading code, not the report) | After implementer, before code-quality-reviewer |
| **code-quality-reviewer** | Code quality, architecture, production readiness | Only after spec compliance is ✅ |

**Order:** Implementer → Spec reviewer (fix until ✅) → Code quality reviewer (fix until approved) → next task.

Code-quality-reviewer follows the template at [../requesting-code-review/code-reviewer.md](../requesting-code-review/code-reviewer.md); the parent supplies WHAT_WAS_IMPLEMENTED, PLAN_OR_REQUIREMENTS, BASE_SHA, HEAD_SHA, DESCRIPTION.

---

## Related skills

- **writing-plans** — Produces the plan this skill executes.
- **using-git-worktrees** — Required: create an isolated workspace before starting.
- **requesting-code-review** — Code-quality-reviewer uses the code-reviewer template.
- **finishing-a-development-branch** — Use after all tasks and final review.

---

## External references

- [Anthropic Agent Skills](https://docs.anthropic.com/en/docs/agents-and-tools/agent-skills/overview) — Skills overview, progressive loading, structure.
- [Claude Code Subagents](https://code.claude.com/docs/en/sub-agents) — Custom subagents in `.claude/agents/`, model options (`haiku`, `sonnet`, `opus`, `inherit`).
- [Cursor Subagents](https://cursor.com/docs/agent/subagents) — `.cursor/agents/` or `.claude/agents/`, foreground vs background.
- [Claude Code Sub-Agents: Best Practices](https://zoer.ai/posts/zoer/claude-code-sub-agents-best-practices) — Three-tier pattern, context budget, validator pattern.
- [Sub-Agent Routing and Invocation](https://claudefa.st/blog/guide/agents/sub-agent-best-practices) — Parallel/sequential/background rules, invocation quality.
