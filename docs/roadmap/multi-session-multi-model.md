# Multi-Session & Multi-Model — Brainstorming Roadmap

> ⚠️ **Planning phase** — not yet implemented. This document captures the vision, design constraints, and open questions.

## Motivation

Today RustFox runs a **single chat session** with **one model at a time**. You `/models` to switch, and the whole bot changes personality.

But power users want:

1. **Multiple concurrent sessions** — work on 3 different projects in 3 different chats, each with independent context.
2. **Different models per session** — code review chat uses Claude Opus, casual chat uses Haiku.
3. **Agent-to-agent delegation** (already partially solved) — one session can ask another session's model to do something.

## Core Concepts

### Session = Chat Thread

Each Telegram chat (DM or group) is a **session**. Sessions are isolated:
- Each has its own conversation history (context window)
- Each can be assigned a different model
- Each can have different system prompts / soul files

### Session Model Binding

| Concept | Description |
|---------|-------------|
| **Default model** | The model used for new sessions (from config) |
| **Session model** | Override for a specific session via `/models` |
| **Session stickiness** | Model choice persists across bot restarts |

### Context Isolation

| Aspect | Shared | Per-Session |
|--------|--------|-------------|
| Soul files (SOUL.md, AGENTS.md, USER.md) | ✅ | ❌ |
| Conversation history | ❌ | ✅ |
| Scheduled tasks | ✅ | ❌ (scheduler is global) |
| Active model | ❌ | ✅ |
| Plans | ❌ | ✅ (per-session plan tracking) |
| Skills | ✅ | ❌ |

## Potential Approach

### Option A: Stateless Sessions

- Session state lives entirely in Telegram (message history replayed on context window overflow)
- No per-session persistence — just model binding
- **Pros**: Simple, no new infrastructure
- **Cons**: Context window limited to recent messages, slow startup replay

### Option B: Lightweight Session Store

- SQLite table `sessions (chat_id, model_name, context_summary, last_active)`
- On each new message, load session state, inject context summary, respond
- **Pros**: Context summaries survive restarts, faster than full replay
- **Cons**: New persistence complexity, summary drift

### Option C: Full Context Persistence

- Store full conversation vector embeddings + message history in SQLite
- RAG-style retrieval at the start of each turn
- **Pros**: Maximum context recall
- **Cons**: Heavy infrastructure, token cost, complexity

## Open Questions

1. **Session lifecycle**: When does a session "expire"? After inactivity? Never?
2. **Cross-session memory**: Should sessions share learned facts (RLHF-style)?
3. **SQLite schema**: How does session state compose with the existing learning/scheduler tables?
4. **Cost tracking**: Per-session token usage & cost reporting?
5. **Billing model**: Some users may want per-session billing (e.g., `gpt-4` costs more than `haiku`).
6. **Migration path**: Users upgrading from single-model shouldn't lose their config.

## Related Work

- `invoke_agent` / `spawn_agents` — already supports subagent isolation
- `/models` command — already supports runtime model switching
- Soul files — already support persistent identity

## Next Steps

1. Choose an approach (A, B, or hybrid)
2. Design SQLite schema if needed
3. Scope implementation into trackable issues
4. Plan migration path for existing users