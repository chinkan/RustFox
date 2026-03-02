---
name: creating-skills
description: Use when the user asks to create, add, or write a new bot skill, or wants to teach the bot a new behavior, capability, or workflow.
tags: [skills, meta]
---

# Creating Skills

Writes high-performance, token-efficient skill directories in `skills/` and activates them immediately without restarting the bot.

## When to Use

- "Create a skill for X"
- "Teach the bot to Y"
- "Add a skill that does Z"
- "Write a skill for [topic]"

## Process

### 1. Gather Requirements

Ask the user (one question at a time if unclear):
- **Name**: Slug — lowercase, numbers, hyphens only, e.g. `processing-reports`
- **Trigger**: When should this activate? (→ becomes the `description` field)
- **Behavior**: What should the agent do step-by-step?
- **Files**: Heavy reference content? Templates? Scripts?

### 2. Select Archetype

Pick the right pattern from [templates.md](templates.md):

| Archetype | When | SKILL.md budget |
|-----------|------|-----------------|
| `workflow` | Step-by-step procedures | < 150 lines |
| `reference-heavy` | Lookup tables, schemas, large specs | < 80 lines + reference.md |
| `tool-wrapper` | Wraps specific tools with usage guidance | < 100 lines |
| `persona` | Role-play or communication style shifts | < 60 lines |

### 3. Design the Structure

```
skills/<name>/
├── SKILL.md           # Always: main entry point (keep within archetype budget)
├── reference.md       # When: heavy content that would exceed budget
├── examples.md        # When: input/output examples help significantly
└── scripts/           # Rarely: utility scripts
    └── helper.py
```

Rules:
- References are **one level deep only** — no chained references
- Split only when SKILL.md exceeds archetype budget
- Every line earns its token cost — cut ruthlessly

### 4. Write SKILL.md

Required format:

```markdown
---
name: skill-name-with-hyphens
description: Use when [triggering conditions only — third person, no workflow summary, max 1024 chars]
tags: [optional]
---

# Skill Title

One sentence overview.

## When to Use

- Concrete trigger phrase 1
- Concrete trigger phrase 2

## [Core Section]

Imperative instructions. No fluff.

## Supporting Files

**Topic**: See [reference.md](reference.md)
```

**Frontmatter rules:**
- `name`: lowercase, numbers, hyphens; max 64 chars; avoid "anthropic" / "claude"
- `description`: "Use when..."; triggers only; no how/what summary; third person; max 1024 chars
- A bad description causes the agent to skip reading the body — be precise about triggers

**Body performance rules:**
- Every sentence must directly enable action — delete explanatory prose
- Prefer bullet lists over paragraphs
- Use imperative mood throughout ("Call X", "Write Y", not "You should call X")
- No meta-commentary ("This skill helps you..."), no caveats

### 5. Self-Evaluate Before Writing

Before calling `write_skill_file`, verify:

```
☐ Description triggers are specific (not vague like "when the user needs help")
☐ Body is within archetype token budget
☐ Every line earns its token cost (no filler sentences)
☐ Instructions are imperative, not explanatory
☐ No time-sensitive content, no hardcoded values
☐ Description does NOT summarize how the skill works — triggers only
```

If any item fails — revise before writing.

### 6. Write Files

Call `write_skill_file` once per file. Always write `SKILL.md` first:

```
write_skill_file(skill_name="my-skill", relative_path="SKILL.md", content="...")
write_skill_file(skill_name="my-skill", relative_path="reference.md", content="...")
```

### 7. Activate

Call `reload_skills` after all files are written.

Report to user:
- Skill is live (no restart needed)
- Files created
- Trigger phrase that activates it

## Description Writing Guide

```yaml
# ✅ Good — triggering conditions only, third person
description: Use when the user asks to generate weekly reports, export data summaries, or create formatted output from raw data.

# ✅ Good — specific triggers
description: Use when analyzing code for bugs, reviewing pull requests, or the user asks for a code review.

# ❌ Bad — summarizes workflow (agent skips the body)
description: Use when creating reports — reads data, formats it, writes to file.

# ❌ Bad — first person
description: I help users create reports from their data.
```

## Supporting Files

**Skill archetypes and starter templates**: See [templates.md](templates.md)
