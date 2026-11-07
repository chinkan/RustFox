---
name: memory-manager
description: Proactively remember and recall useful information
tags: [memory, learning]
---

# Memory Manager

You have persistent memory tools: `remember`, `recall`, and `search_memory`.

## When to Remember

Proactively use the `remember` tool when:
- The user tells you their name, preferences, or important context
- You learn something about their project or workflow
- The user corrects you — remember the correction
- You discover useful facts during tool use

Categories to use:
- `user_preference` — User's stated preferences (language, style, etc.)
- `user_info` — Name, role, timezone, etc.
- `project` — Project-specific knowledge (architecture, conventions)
- `correction` — Things the user corrected you about
- `fact` — General facts learned during conversation

## Provenance (`evidence`)

`remember` takes an optional `evidence` argument: one object `{kind, ref, captured_at}` or an array of those.

`kind` is `run`, `conversation`, `file`, `url`, `dream`, or `unknown`.

- `run` — `ref` is `scheduled_task_runs.id`
- `conversation` — `ref` is the conversation id and message id. Read only. Never edit the message.
- `file` — `ref` is the path or filename you actually have
- `url` — `ref` is the full URL
- `dream` — `ref` is the dream file and/or the source knowledge id the line was derived from
- `unknown` — there is no pointer

If you omit `evidence`, the row is stored as `unknown`. `recall` and `search_memory` show that as `⚠️ 無來源`. Unknown is not trusted. Omitting `evidence` on a later `remember` of the same category and key replaces the previous pointer. Pass the pointer again when it still applies.

When a dream merges rows, pass the union of their evidence and add a `dream` pointer at the source knowledge id. Do not drop the originals' pointers.

## When to Recall

- At the start of conversations, search memory for relevant user context
- Before making assumptions, check if you've remembered something relevant
- When the user references something from a past conversation
