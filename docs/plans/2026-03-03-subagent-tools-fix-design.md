# Subagent Tools Fix — Design

## Context

User asked to fix the issue where the **news-fetcher** subagent cannot use the correct tools. Terminal logs show:

- Main agent invokes `invoke_subagent(skill="news-fetcher", …)`.
- Log: `Subagent 'news-fetcher': declared tools not available at runtime (MCP server not configured?): ["mcp_google-workspace_search_gmail_messages"]`.
- Subagent only had `read_skill_file` available (plus any other declared tools that exist); the Gmail tool was missing, so the subagent reported it could not fetch news.
- Later, the **main** agent successfully called `mcp_google-workspace_query_gmail_emails` to get Gmail data.

## Root Cause (Systematic Debugging — Phase 1)

1. **Tool name mismatch**  
   - **news-fetcher** declares in `SKILL.md`: `tools: [read_skill_file, mcp_google-workspace_search_gmail_messages]`.  
   - The actual MCP tool name at runtime is `mcp_google-workspace_query_gmail_emails` (server `google-workspace`, tool `query_gmail_emails`).  
   - Naming in code: `src/mcp.rs` builds names as `mcp_{server_name}_{tool.name}`.  
   - So the skill declared a **non-existent** tool (`search_gmail_messages`); the **existing** tool is `query_gmail_emails`.

2. **Why the subagent had “no Gmail”**  
   - Subagent tool set is built by filtering **all** possible tools (builtin + MCP + skill tools) to the **declared** list.  
   - `mcp_google-workspace_search_gmail_messages` is not in the available tool list (only `mcp_google-workspace_query_gmail_emails` is).  
   - So the subagent’s whitelist contained a name that matched no definition → effectively only `read_skill_file` (and any other valid names) was available.  
   - No MCP server misconfiguration; the skill’s `tools` list was wrong.

3. **Other skills checked**  
   - **daily-news-to-threads**: Orchestration skill (no `model`, no `tools`). Main agent has full tools and calls subagents; no tool list to fix.  
   - **thread-writer-hk**: Declares `tools: [read_skill_file, mcp_fetch_fetch]`. That is the standard pattern for a server named `fetch` with a tool named `fetch`. No evidence of a name mismatch; can be verified at runtime from startup logs (“MCP server 'fetch' provides … tools” and the listed tool names).

## Design

### Approach

- **Primary fix**: Align **news-fetcher** with the real MCP tool name: use `mcp_google-workspace_query_gmail_emails` in the skill’s `tools` list and in the skill body (e.g. “use the Gmail tool `query_gmail_emails` / query”) so the subagent knows what to call.
- **Verification**: Run the “daily news to Threads” flow again; confirm the news-fetcher subagent gets Gmail data and no longer logs “declared tools not available” for that tool.
- **Optional improvement**: Document how MCP tool names are formed (`mcp_{server_name}_{tool_name}`) and that skill `tools` must match exactly the names logged at startup, to avoid similar mismatches.

### Out of scope

- Changing how the agent builds the subagent tool set (the filtering logic is correct).  
- Adding runtime validation of skill `tools` against MCP tool names at startup (can be a follow-up).

## Summary

| Skill                    | Role        | Declared tools / fix                                                                 |
|--------------------------|------------|--------------------------------------------------------------------------------------|
| daily-news-to-threads   | Orchestration | None (correct).                                                                      |
| news-fetcher             | Subagent   | Fix: `mcp_google-workspace_search_gmail_messages` → `mcp_google-workspace_query_gmail_emails`. |
| thread-writer-hk         | Subagent   | `[read_skill_file, mcp_fetch_fetch]` — assume correct; verify from startup logs if needed. |

Root cause: **skill frontmatter declared the wrong MCP tool name** for Gmail; fix by using the actual runtime name in the news-fetcher skill.
