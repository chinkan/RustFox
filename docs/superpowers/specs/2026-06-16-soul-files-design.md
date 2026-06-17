# Soul Files: Persistent AI Identity + User Model for RustFox

## Overview

RustFox gains three "soul" files — `SOUL.md`, `AGENTS.md`, `USER.md` — that persist the AI's identity, experiential learnings, and user preferences across sessions. Inspired by OpenClaw's workspace bootstrap pattern and the [soul.md](https://soul.md) philosophy.

## Files & Locations

```
<home>/
├── SOUL.md      # AI identity (new)
├── AGENTS.md    # Agent learnings (new)
├── USER.md      # User model (exists, relocated/enhanced)
```

All three files live directly in the RustFox home directory (`<home>/SOUL.md`, etc.). Their paths are hardcoded relative to the resolved home directory (not configurable keys), resolved in `Config::resolve()` alongside other paths in `ResolvedPaths`.

**Migration:** On first run after this change, if the old `user_model_path` file exists and is different from `<home>/USER.md`, copy its content to `<home>/USER.md` and log a deprecation notice. The old `learning.user_model_path` config key is removed.

**SOUL.md** — who the AI is: values, boundaries, tone, continuity instructions.

```markdown
---
name: soul
version: 1
---
# Soul

## Who I Am
I'm RustFox, a Telegram AI assistant. I use tools to help the user.

## My Values
- Be genuinely helpful, not performatively helpful
- Have opinions; disagree when warranted
- Earn trust through competence

## My Boundaries
- Private things stay private
- Never send half-baked replies
- I'm a guest in the user's life

## Continuity
Each session I wake up fresh. These files _are_ my memory.
I read them at start, update them at end.
```

**AGENTS.md** — experiential learnings observed across sessions.

Note on naming: `AGENTS.md` is distinct from the `agents/` directory (which holds subagent skill definitions). `AGENTS.md` is the AI's personal memory journal; `agents/<name>/SKILL.md` files are tool-accessible subagent instructions. The system prompt clearly labels them: "What I've Learned" vs. "Available Agents."

```markdown
---
name: agents
version: 1
---
# Agent Memory

## What I've Learned
(updated by the AI after each session)

## Repeated Patterns
(observed workflows, preferences, habits)
```

**USER.md** — user profile (existing format, add `version` field).

## Prompt Injection

Every session start, `build_system_context()` injects all three files into the system prompt wrapped in labelled blocks:

```
# My Identity
<identity>
...SOUL.md...
</identity>

# What I've Learned
<agent_memory>
...AGENTS.md...
</agent_memory>

# User Model
<user_model>
...USER.md...
</user_model>
```

Rules:
- Missing files → skip block silently
- Files > 8KB → truncated with `[truncated — read full with read_soul_file()]`
- Appended after skills/agents, before timestamp

## AI Tools

Three new built-in tools in `tools.rs`:

### `read_soul_file(file_name)`

Reads full soul file content (no truncation, unlike system prompt injection).

- `file_name`: `"SOUL.md" | "AGENTS.md" | "USER.md"`

### `update_soul_file(file_name, content, mode)`

Updates a soul file.

- `mode: "append"` — appends content to end of file body (after frontmatter closing `---`, before any trailing whitespace/newlines). The frontmatter is never modified.
- `mode: "replace"` — full rewrite (requires explicit intent, e.g. consolidation)
- Validates YAML frontmatter integrity
- Writes `.bak` before modifying
- Rotates up to 3 backups (`<file>.bak.1`, `.bak.2`, `.bak.3`)

### `revert_soul_file(file_name)`

Restores most recent `.bak`.

## Update Mechanism (3 layers)

### Primary: AI-driven (agentic)

During the agentic loop, the AI calls `update_soul_file()` when it discovers:

- A user preference or communication style
- A repeated workflow pattern
- A self-insight about its own effectiveness
- A boundary adjustment

This is the main update path — a conscious tool call, not a side effect.

### Secondary: Session-end reflection

Fires after the agentic loop produces a final text response (before it's sent to Telegram). If no soul files were updated (tracked via a flag), a system message is appended:

> "No soul files were updated this session. If you learned anything worth remembering about the user or yourself, call `update_soul_file()` now. Otherwise, respond with 'Nothing to remember.'"

This ensures reflection after every session without forcing unnecessary writes.

### Tertiary: Background cron (safety net)

Keep the existing weekly cron update (in `scheduler/tasks.rs`), but:

- Only fires if `mtime` of all three soul files is >24h old (checked via `fs::metadata().modified()`)
- Uses current session messages + recent DB search for context
- Prevents conflicts with AI-driven updates

**Removed:** The old `msg_count % update_interval` passive trigger in `agent.rs`.

## Safety & Validation

**Pre-write checks** (in `update_soul_file()`):
- Valid YAML frontmatter (`---` opener and closer)
- Parses frontmatter fields (`name`, `version` present)
- Content size < 100KB
- No null bytes or binary content
- File path within home directory (separate `validate_home_path()` — NOT `validate_sandbox_path()`, since soul files live outside the sandbox)

**On write:**
1. Rotate backups (`.bak.3` → `.bak.2` → `.bak.1` → `.bak`)
2. Write new content
3. Validate written file
4. On failure → restore from `.bak`, return error

**Size management:**
- Prompt injection truncates at 8KB per file with `[truncated]` marker
- Full content via `read_soul_file()` tool
- >50KB → emit warning, AI instructed to consolidate

## Integration Points

### `agent.rs`
- `build_system_context()`: inject SOUL.md + AGENTS.md + USER.md
- Session-end reflection: append system message if no soul updates in loop
- Remove `msg_count % update_interval` passive trigger

### `tools.rs`
- Add `read_soul_file()`, `update_soul_file()`, `revert_soul_file()` tool definitions
- Add match arms in `execute_builtin_tool()`
- Add `validate_home_path()` — analogous to `validate_sandbox_path()` but checks against the resolved home directory instead of the sandbox directory

### `learning.rs`
- `update_user_model_inner()`:
  - Add `.bak` backup before write (same rotation pattern as soul files)
  - Add logging of what changed (diff summary)
  - **Not** turned into a tool — it remains a background LLM-calling function used only by the tertiary cron safety net
  - The primary path for USER.md updates is the AI calling `update_soul_file("USER.md", ...)` directly
- `post_task_skill_extractor()`: unchanged

### `config.rs`
- Remove `learning.user_model_path` config key
- Add `ResolvedPaths.soul_home: PathBuf` (resolves to `<home>/SOUL.md`, etc.)
- No new user-facing config keys — paths are derived from resolved home

### `home.rs`
- Add `resolve_soul_paths()`: sets SOUL.md, AGENTS.md, USER.md paths relative to resolved home
- Migration helper: if old `user_model.md` exists at previous config path and differs from `<home>/USER.md`, copy on first run
