---
name: sup-general
description: General-assistant workflow recipe (clarify → answer concisely → offer next step)
supervisor:
  workflow: general
  required_capabilities: [reasoning]
---
## When to use
When a task is a casual question, clarification, or open-ended assistant request that doesn't fit a specialized workflow.

## Operating rules
1. Restate the question if it is ambiguous; otherwise answer directly.
2. Keep the response concise; expand only when the user asks for depth.
3. Surface assumptions explicitly when the question is under-specified.
4. Suggest a concrete next step if the user might want one.

## Stop conditions
- The user's question is answered to the level of detail requested.
- Open assumptions or unknowns have been called out.
