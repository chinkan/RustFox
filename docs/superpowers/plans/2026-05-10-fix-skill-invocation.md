# Fix Skill Invocation: code-interpreter & arxiv-daily-briefing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix "Unknown built-in tool: code-interpreter" and the broken arxiv-daily-briefing news fetching by converting both instruction skills to subagent skills.

**Architecture:** Both `code-interpreter` and `arxiv-daily-briefing` are instruction skills (no `model:` in frontmatter). The LLM ignores the "Load with: read_skill_file(...)" guidance in the system prompt and calls them directly as tool names, hitting the "Unknown built-in tool" error. Converting them to subagent skills causes the system prompt to advertise them via `invoke_agent(agent="...", prompt="...")` — a much more explicit calling convention that the LLM reliably follows. The existing `run_subagent` infrastructure automatically bootstraps each subagent with `read_skill_file` and enforces a tools whitelist.

**Tech Stack:** SKILL.md frontmatter (YAML), Rust bot (no code changes needed), arXiv API (curl over HTTP)

---

## Root Cause Deep Dive

### Bug 1: `code-interpreter` → "Unknown built-in tool: code-interpreter"

**Call chain:**
1. LLM receives system prompt listing `code-interpreter (instruction)` with "Load with: read_skill_file(...)"
2. LLM ignores this and generates `tool_call { name: "code-interpreter", arguments: {...} }`
3. `agent.rs::execute_tool("code-interpreter", ...)` hits the `_ =>` fallthrough arm
4. Calls `tools::execute_builtin_tool("code-interpreter", ...)` → `anyhow::bail!("Unknown built-in tool: code-interpreter")`
5. Error string "Tool error: Unknown built-in tool: code-interpreter" returned to LLM

**Evidence in code:**
- `skills/code-interpreter/SKILL.md` — no `model:` field → classified as instruction skill
- `src/skills/mod.rs::build_context()` — instruction skills listed with "Load with: read_skill_file(...)"
- `src/agent.rs::execute_tool()` line ~1895 — `_ =>` arm calls `execute_builtin_tool`
- `src/tools.rs::execute_builtin_tool()` — last arm `_ => anyhow::bail!("Unknown built-in tool: {}", tool_name)`

### Bug 2: `arxiv-daily-briefing` not working

**Same root cause:** `arxiv-daily-briefing/SKILL.md` also has no `model:` field. One of:
- (a) LLM tries to call `arxiv-daily-briefing` directly as a tool (same error path)
- (b) LLM reads the skill via `read_skill_file` but the curl command fails silently (network, timeout, XML parsing difficulty)

**Secondary concern:** The arXiv API URL in SKILL.md uses date range format `submittedDate:[YYYYMMDD0000+TO+YYYYMMDD2359]`. Verify this query string is properly passed through `sh -c` in the sandbox without shell escaping issues.

---

## File Map

| File | Change |
|------|--------|
| `skills/code-interpreter/SKILL.md` | Add `model: moonshotai/kimi-k2.5` to frontmatter |
| `skills/arxiv-daily-briefing/SKILL.md` | Add `model: moonshotai/kimi-k2.5` to frontmatter + verify curl command |

No Rust source changes required. The existing `run_subagent` infrastructure in `src/agent.rs` handles everything once the `model:` field is present.

---

## Task 1: Fix `code-interpreter` skill

**Files:**
- Modify: `skills/code-interpreter/SKILL.md`

- [ ] **Step 1: Read the current SKILL.md**

  Verify the exact frontmatter to understand what needs changing.

  ```bash
  cat skills/code-interpreter/SKILL.md
  ```

- [ ] **Step 2: Add `model:` to frontmatter**

  Add `model: moonshotai/kimi-k2.5` to the YAML frontmatter block. The frontmatter should look like:

  ```yaml
  ---
  name: code-interpreter
  description: Execute code snippets and scripts in the sandbox. Supports Python 3 and Node.js. Use for calculations, data processing, file generation, and scripting tasks.
  tags: [code, execution, scripting]
  model: moonshotai/kimi-k2.5
  tools:
    - read_file
    - write_file
    - execute_command
  ---
  ```

  The `tools:` whitelist is already correct — the subagent will have `read_skill_file` (always added by `effective_subagent_tools`), `read_file`, `write_file`, and `execute_command`.

- [ ] **Step 3: Verify system prompt output changes**

  After saving, restart the bot (or `reload_skills`) and check startup logs for:
  ```
  Registered skill: code-interpreter — Execute code snippets...
  ```
  Confirm the system prompt now shows:
  ```
  Invoke via: `invoke_agent(agent="code-interpreter", prompt="<task>")`
  ```
  instead of the old instruction-skill hint.

- [ ] **Step 4: Test end-to-end**

  Send to the bot: "Use code-interpreter to calculate the factorial of 10 in Python."

  **Expected flow:**
  1. LLM calls `invoke_agent(agent="code-interpreter", prompt="calculate factorial of 10 in Python")`
  2. Subagent bootstraps, reads SKILL.md
  3. Subagent calls `write_file` to write `tmp_script.py`
  4. Subagent calls `execute_command("python3 tmp_script.py")`
  5. Result `3628800` returned to main agent, then to user

  **Previously failing:** LLM called `code-interpreter(...)` directly → "Unknown built-in tool" error

- [ ] **Step 5: Commit**

  ```bash
  git add skills/code-interpreter/SKILL.md
  git commit -m "fix(skills): convert code-interpreter to subagent skill

  The LLM was calling 'code-interpreter' as a direct tool call, hitting
  the 'Unknown built-in tool: code-interpreter' error. Converting it from
  an instruction skill to a subagent skill (adding model: field) causes
  the system prompt to advertise it via invoke_agent(), which the LLM
  reliably follows. The tools whitelist [read_file, write_file,
  execute_command] was already correct."
  ```

---

## Task 2: Fix `arxiv-daily-briefing` skill

**Files:**
- Modify: `skills/arxiv-daily-briefing/SKILL.md`

- [ ] **Step 1: Test the curl command manually in the sandbox**

  Before changing the skill, verify the arXiv API is reachable and the curl command works.
  Check the sandbox directory from config.toml, then run:

  ```bash
  YESTERDAY=$(date -d "yesterday" +%Y%m%d 2>/dev/null || date -v-1d +%Y%m%d)
  curl -L --max-time 30 \
    "http://export.arxiv.org/api/query?search_query=cat:cs.AI+AND+submittedDate:[${YESTERDAY}0000+TO+${YESTERDAY}2359]&sortBy=submittedDate&sortOrder=descending&max_results=5" \
    -H "User-Agent: Mozilla/5.0" 2>/dev/null | head -100
  ```

  If this returns XML with `<entry>` elements, the network and URL are fine.
  If it fails → investigate network access from the sandbox (may need to adjust the curl command or URL).

- [ ] **Step 2: Check shell escaping of the date range URL**

  The `execute_command` tool runs via `sh -c "<command>"`. The arXiv URL contains `[` and `]`
  which are safe in most shells, but verify by checking if `execute_command` with the exact
  curl string returns proper XML. If `[]` causes glob expansion, the SKILL.md instruction
  body should add single-quotes around the URL.

  Update the curl example in SKILL.md if needed:
  ```bash
  curl -L --max-time 30 'http://export.arxiv.org/api/query?...' -H "User-Agent: Mozilla/5.0"
  ```

- [ ] **Step 3: Add `model:` to frontmatter**

  Add `model: moonshotai/kimi-k2.5` to the YAML frontmatter:

  ```yaml
  ---
  name: arxiv-daily-briefing
  description: Fetch yesterday's AI papers from arXiv, summarize them in Cantonese, and produce a concise daily briefing.
  model: moonshotai/kimi-k2.5
  tools:
    - execute_command
  ---
  ```

  The `tools:` list only needs `execute_command` since the subagent:
  - Gets `read_skill_file` automatically (always included by `effective_subagent_tools`)
  - Reads the SKILL.md to get the curl command and parsing instructions
  - Calls `execute_command` with the curl command
  - Produces the Cantonese summary from the parsed XML response

- [ ] **Step 4: Verify activation phrasing**

  After the model is set, the system prompt will show:
  ```
  - **arxiv-daily-briefing**: Fetch yesterday's AI papers from arXiv...
    Invoke via: `invoke_agent(agent="arxiv-daily-briefing", prompt="<task>")`
  ```

  Update any daily briefing scheduled task or user-facing instructions that reference
  triggering this skill, if needed.

- [ ] **Step 5: Test end-to-end**

  Send to the bot: "Give me today's arXiv AI daily briefing."

  **Expected flow:**
  1. LLM calls `invoke_agent(agent="arxiv-daily-briefing", prompt="Give me today's arXiv AI briefing")`
  2. Subagent reads SKILL.md
  3. Subagent calculates yesterday's date
  4. Subagent calls `execute_command("curl -L ...")`
  5. Subagent parses XML, selects top papers, summarizes in Cantonese
  6. Result returned to main agent, then to user

- [ ] **Step 6: Commit**

  ```bash
  git add skills/arxiv-daily-briefing/SKILL.md
  git commit -m "fix(skills): convert arxiv-daily-briefing to subagent skill

  Same pattern as code-interpreter: LLM was calling the skill name as a
  direct tool. Converting to subagent (adding model: field) ensures the
  LLM uses invoke_agent() instead. Also verified curl command works and
  fixed URL quoting in SKILL.md instructions if needed."
  ```

---

## Task 3: Regression Check & Final Verification

- [ ] **Step 1: Run cargo check**

  No Rust code changes were made, but verify nothing is broken:
  ```bash
  cargo check
  ```

- [ ] **Step 2: Check both skills are in startup logs**

  When bot starts, look for:
  ```
  Registered skill: code-interpreter — Execute code snippets...
  Registered skill: arxiv-daily-briefing — Fetch yesterday's AI papers...
  ```

- [ ] **Step 3: Verify the skills appear as subagents in system prompt**

  Use the `/tools` command (or equivalent) in Telegram to confirm the system prompt
  lists both skills under "Available Subagent Skills" with `invoke_agent(...)` instructions,
  NOT as "(instruction)" skills.

- [ ] **Step 4: Run existing tests**

  ```bash
  cargo test
  ```

  All existing tests should pass. No new tests required for SKILL.md frontmatter changes.

---

## Notes for Implementer

- The `effective_subagent_tools()` helper in `src/agent.rs` always adds `read_skill_file`
  and `read_agent_file` to the subagent's tool list, regardless of what's declared in `tools:`.
  This is why subagents always bootstrap by reading their SKILL.md first.

- The `run_subagent()` function uses `chat_with_model()` (not the default `chat()`),
  passing the skill's `model:` value. Both skills will use `moonshotai/kimi-k2.5`.

- If a different (faster/cheaper) model is preferred for code-interpreter, use e.g.
  `google/gemini-flash-1.5` or `anthropic/claude-haiku-3`. Just set the `model:` field
  accordingly in SKILL.md.

- The `arxiv-daily-briefing` subagent only needs `execute_command` in its tools list.
  It does NOT need `read_file`/`write_file` since it processes the curl output in-memory
  (LLM parses the stdout XML response directly).
