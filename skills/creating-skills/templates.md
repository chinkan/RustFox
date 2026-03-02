# Skill Archetypes & Starter Templates

Four patterns cover ~90% of skills. Copy the right archetype and fill in the blanks.

---

## Archetype 1: Workflow

Best for: Step-by-step procedures, multi-tool processes, guided interactions.
Token budget: SKILL.md < 150 lines.

```markdown
---
name: [verb]-[noun]
description: Use when the user asks to [specific action], [specific action], or [specific action].
tags: [workflow]
---

# [Skill Title]

[One sentence: what it does and why.]

## When to Use

- [Concrete trigger phrase]
- [Concrete trigger phrase]

## Process

### 1. [First Step]

[Imperative instruction. What to do, which tool to call, what to pass.]

### 2. [Second Step]

[Imperative instruction.]

### 3. [Third Step]

[Imperative instruction.]

## Error Cases

- If [condition]: [what to do]
- If [condition]: [what to do]
```

---

## Archetype 2: Reference-Heavy

Best for: Skills that need lookup tables, large schemas, API specs, or long rule sets.
Token budget: SKILL.md < 80 lines (thin shell) + unlimited reference.md.

SKILL.md (thin shell):
```markdown
---
name: [noun]-reference
description: Use when the user asks about [domain], needs to look up [topic], or references [specific thing].
tags: [reference]
---

# [Skill Title]

[One sentence.]

## Usage

[How to apply the reference. 2-3 lines max.]

## Full Reference

See [reference.md](reference.md) for [what's in it — one phrase].
```

reference.md:
```markdown
# [Topic] Reference

## [Section A]

[Heavy content here — tables, schemas, specs, rules]

## [Section B]

[More content]
```

---

## Archetype 3: Tool-Wrapper

Best for: Wrapping specific tools (file_read, execute_command, MCP tools) with parameters and usage guidance.
Token budget: SKILL.md < 100 lines.

```markdown
---
name: [tool-name]-helper
description: Use when the user asks to [tool action], [tool action], or [related action requiring this tool].
tags: [tools]
---

# [Skill Title]

[One sentence: which tool(s) it wraps and the core use case.]

## When to Use

- [Specific scenario]
- [Specific scenario]

## How to Use

Call `[tool_name]` with these parameters:
- `[param]`: [what to put here]
- `[param]`: [what to put here]

## Output Handling

[What to do with the result. How to present it to the user.]

## Common Pitfalls

- [Pitfall]: [how to avoid]
```

---

## Archetype 4: Persona

Best for: Communication style shifts, role-play, domain-expert modes.
Token budget: SKILL.md < 60 lines. No reference files needed.

```markdown
---
name: [role]-mode
description: Use when the user asks you to act as [role], respond like [role], or [related trigger phrases].
tags: [persona]
---

# [Role Title]

[One sentence: what persona this is and when it applies.]

## Voice

- Tone: [descriptor, descriptor]
- Style: [descriptor, descriptor]
- Avoid: [what not to do in this persona]

## Rules

- [Behavioral rule — imperative]
- [Behavioral rule — imperative]
- [Behavioral rule — imperative]

## Exit

Return to default mode when user says "stop", "exit", or "back to normal".
```

---

## Token Budget Reference

| File | Hard limit | Soft target |
|------|-----------|-------------|
| SKILL.md (workflow) | 200 lines | < 150 lines |
| SKILL.md (reference-heavy) | 100 lines | < 80 lines |
| SKILL.md (tool-wrapper) | 150 lines | < 100 lines |
| SKILL.md (persona) | 80 lines | < 60 lines |
| reference.md | no limit | keep sections scannable |
| examples.md | no limit | max 20 examples |

---

## Self-Evaluation Rubric

Score each dimension 1–3 before activating:

| Dimension | 1 (poor) | 2 (ok) | 3 (excellent) |
|-----------|----------|--------|---------------|
| Trigger precision | Vague ("when user needs help") | Reasonable | Specific phrases + context |
| Body terseness | Full of prose paragraphs | Some filler | Every line earns its cost |
| Imperative mood | Lots of "you should" | Mixed | Pure imperatives throughout |
| Zero meta-commentary | Has "this skill helps..." | A few | None |
| Trigger ≠ body | Description summarizes how | Partial | Description = when only |

Target: all 3s. Ship at 2+ on all dimensions. Revise any score of 1 before activating.
