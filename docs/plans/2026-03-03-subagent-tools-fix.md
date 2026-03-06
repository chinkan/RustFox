# Subagent Tools Fix — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Fix the news-fetcher subagent so it receives the correct Gmail MCP tool by declaring the actual runtime tool name in its skill frontmatter and instructions.

**Architecture:** Skill frontmatter `tools` is a whitelist of exact tool names. MCP tools are named `mcp_{server_name}_{tool_name}`. The google-workspace server exposes `query_gmail_emails`, so the full name is `mcp_google-workspace_query_gmail_emails`. No code changes; only skill content and optional docs.

**Tech Stack:** YAML frontmatter in Markdown (skills loader), existing agent/MCP tool filtering.

---

## Task 1: Fix news-fetcher SKILL.md frontmatter and body

**Files:**
- Modify: `skills/news-fetcher/SKILL.md`

**Step 1: Update the `tools` list in frontmatter**

Change line 5 from:
```yaml
tools: [read_skill_file, mcp_google-workspace_search_gmail_messages]
```
to:
```yaml
tools: [read_skill_file, mcp_google-workspace_query_gmail_emails]
```

**Step 2: Align the skill body with the real tool name**

In the "Gmail query" section, the skill says "Use the Gmail MCP tool". Optionally add a short note that the tool name is `mcp_google-workspace_query_gmail_emails` (or "query_gmail_emails" on the google-workspace server) so future editors know the exact name. No strict wording change required if the frontmatter is fixed; the subagent will see the tool in its list and the existing instructions are enough.

**Step 3: Commit**

```bash
git add skills/news-fetcher/SKILL.md
git commit -m "fix(skills): news-fetcher use correct Gmail MCP tool name (query_gmail_emails)"
```

---

## Task 2: Verify thread-writer-hk tool name (optional)

**Files:**
- Read: `skills/thread-writer-hk/SKILL.md` (already has `mcp_fetch_fetch`)

**Step 1: Confirm at runtime**

When the bot starts, logs show: `MCP server 'X' provides N tools` and `  - <tool.name>: <description>`. The full name sent to the LLM is `mcp_{server_name}_{tool.name}`. If the fetch server is configured as `name: fetch` and exposes a tool named `fetch`, then `mcp_fetch_fetch` is correct. No change needed unless your config uses a different server name or the tool has a different name (e.g. `fetch_url` → `mcp_fetch_fetch_url`).

**Step 2: If mismatch found**

Update `skills/thread-writer-hk/SKILL.md` frontmatter `tools` to the exact name(s) shown in startup logs, then commit.

---

## Task 3: Document MCP tool naming for skills (optional)

**Files:**
- Create or modify: `docs/plans/` or a short section in `CLAUDE.md` / `skills/README.md` if it exists

**Step 1: Add a short note**

State that subagent skill `tools` in frontmatter must list the **exact** tool names as seen by the agent: for MCP, `mcp_{server_name}_{tool_name}` (e.g. `mcp_google-workspace_query_gmail_emails`). These names are logged at startup when MCP servers connect. No code changes.

**Step 2: Commit (if created/modified)**

```bash
git add <path-to-doc>
git commit -m "docs: document MCP tool naming for skill tools whitelist"
```

---

## Verification

After Task 1:

1. Start the bot with the same config (google-workspace MCP enabled).
2. Trigger the daily-news-to-threads flow (e.g. “幫我出今日 AI 新聞去 Threads”).
3. Confirm logs: no warning `Subagent 'news-fetcher': declared tools not available ... mcp_google-workspace_...`.
4. Confirm the news-fetcher subagent returns Gmail-derived content (or a clear “no results” response) instead of “I don't have access to the required … tool”.

Use @verification-before-completion before marking the fix complete.
