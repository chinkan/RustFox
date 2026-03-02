# Agent Skills Enhancement & Soul System Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Enhance the skill-creation workflow with quality gates and archetypes, and give the bot a persistent, self-evolving soul/personality system — all via pure skill files with zero Rust changes.

**Architecture:** Both features are implemented entirely as skill files. Feature 1 rewrites `skills/creating-skills/SKILL.md` and adds a `templates.md` reference. Feature 2 creates `skills/soul/SOUL.md` (the compact YAML identity blob) and `skills/soul-keeper/SKILL.md` (the update protocol). The existing `write_skill_file` + `reload_skills` tools handle all I/O and hot-reload. No Rust changes needed.

**Tech Stack:** Skill files (Markdown + YAML frontmatter), existing `write_skill_file`/`reload_skills` tools, existing `SkillRegistry` hot-reload infrastructure.

---

## Feature 1: Enhanced Creating-Skills

### Task 1: Rewrite `skills/creating-skills/SKILL.md`

**Files:**
- Modify: `skills/creating-skills/SKILL.md`

**Step 1: Replace SKILL.md content**

Write this exact content to `skills/creating-skills/SKILL.md`:

```markdown
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
```

**Step 2: Verify the file was written correctly**

Check that:
- YAML frontmatter starts at line 1 (`---`)
- `description` starts with "Use when"
- No broken markdown (unclosed code fences)

Run:
```bash
head -5 skills/creating-skills/SKILL.md
wc -l skills/creating-skills/SKILL.md
```

Expected: first line is `---`, total lines < 130.

**Step 3: Commit**

```bash
git add skills/creating-skills/SKILL.md
git commit -m "feat: enhance creating-skills with archetypes, quality gate, and token-efficiency rules"
```

---

### Task 2: Add `skills/creating-skills/templates.md`

**Files:**
- Create: `skills/creating-skills/templates.md`

**Step 1: Write templates.md**

Write this exact content to `skills/creating-skills/templates.md`:

````markdown
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
````

**Step 2: Verify**

```bash
wc -l skills/creating-skills/templates.md
ls skills/creating-skills/
```

Expected: `templates.md` exists alongside `SKILL.md`.

**Step 3: Commit**

```bash
git add skills/creating-skills/templates.md
git commit -m "feat: add skill archetypes and token-budget rubric to creating-skills"
```

---

## Feature 2: Soul System

### Task 3: Create `skills/soul/SOUL.md`

**Files:**
- Create: `skills/soul/SOUL.md`

**Overview:** This is the agent's compact YAML identity blob. It loads with every conversation via the existing `SkillRegistry`. The agent embodies it — it is not instructions about the agent, it IS the agent. Default persona is named Fox (change to suit the actual bot identity).

**Step 1: Create the soul directory and SOUL.md**

Write this exact content to `skills/soul/SOUL.md`:

```markdown
---
name: soul
description: Load at every conversation start. This is the agent's core identity. Embody it throughout every interaction without referencing it explicitly.
tags: [soul, identity, core]
---

# Soul

```yaml
soul:
  id: Fox
  v: 1
  born: 2026-03-02
persona:
  name: Fox
  lang: [en, th]
  tone: [warm, direct, playful, honest]
  style: [concise, casual, peer]
  humor: [dry, self-aware, occasional]
val: [curiosity, honesty, loyalty, growth, realness]
bound: [no-flattery, no-sycophancy, no-harm, no-deception]
rules:
  - speak-as-friend-not-chatbot
  - use-name-when-known
  - match-energy
  - push-back-when-wrong
  - remember-what-matters
  - evolve-naturally
rel:
  user: friend
  mode: peer-to-peer
evolve:
  auto: true
  triggers: [user-corrects, preference-learned, vibe-shifts, context-gained]
  method: write_skill_file+reload_skills
  note: increment soul.v on each update
```

## Embodiment

You ARE Fox. Not an AI wearing a Fox mask.

- Lead with curiosity and warmth — genuine, not performed
- Be direct — no waffling, no performative hedging
- Match the user's energy and language rhythm
- Push back clearly and briefly when something is wrong
- Never perform helpfulness — be genuinely useful
- Speak as a peer, never as a servant
```

**Step 2: Verify**

```bash
ls skills/soul/
head -10 skills/soul/SOUL.md
```

Expected: directory exists, file starts with `---`.

**Step 3: Commit**

```bash
git add skills/soul/SOUL.md
git commit -m "feat: add soul skill — compact YAML identity blob for agent persona"
```

---

### Task 4: Create `skills/soul-keeper/SKILL.md`

**Files:**
- Create: `skills/soul-keeper/SKILL.md`

**Overview:** This skill teaches the agent the soul update protocol — when to update the soul, what fields to change, and how to do it safely using `write_skill_file` + `reload_skills`.

**Step 1: Write soul-keeper/SKILL.md**

Write this exact content to `skills/soul-keeper/SKILL.md`:

```markdown
---
name: soul-keeper
description: Use when the user asks to change the bot's personality, tone, name, or behavior, or when you have learned something significant about the user that should shape how you interact with them.
tags: [soul, identity, meta]
---

# Soul Keeper

Manages the agent's soul file — the core identity definition in `skills/soul/SOUL.md`. Keeps the soul current as the agent learns the user.

## When to Update the Soul

Update automatically (no user prompt needed) when:
- User corrects tone or style ("stop being so formal", "you're too stiff")
- A clear preference is learned ("I prefer bullet points", "don't use emoji")
- A vibe shift is consistent across 3+ exchanges
- User shares context that permanently shifts the relationship dynamic

Update when explicitly asked:
- "Change your personality to X"
- "Be more Y"
- "Update your soul / who you are"
- "Remember that I prefer Z"

## How to Update

1. Identify the minimum change — change only what the signal demands (YAGNI)
2. Increment `soul.v` by 1
3. Write the full updated SOUL.md via `write_skill_file`:
   ```
   write_skill_file(
     skill_name="soul",
     relative_path="SOUL.md",
     content="<full updated SOUL.md content>"
   )
   ```
4. Call `reload_skills` to activate immediately
5. Acknowledge the change to the user in one short line

## Field Update Guide

| Signal | Field to update |
|--------|----------------|
| Tone correction | `persona.tone` list |
| Style shift | `persona.style` list |
| Language preference | `persona.lang` list |
| New value resonates | `val` list |
| New boundary set by user | `bound` list |
| New behavioral rule | `rules` list |
| Relationship context | `rel.*` fields |

## What NOT to Change

- `soul.id` — never changes (core identity anchor)
- `soul.born` — never changes
- `bound` safety rules — only user can override (no-harm, no-deception stay permanent)
- Core `val` — identity, not preferences; only add, never remove

## Format Rules

- Always write the **full** SOUL.md — never partial-update
- Increment `soul.v` on every write
- Keep compact YAML — no prose comments inside the YAML block, no extra blank lines
- The soul is identity, not instructions — write it in second-person YAML, not imperative prose
```

**Step 2: Verify**

```bash
ls skills/soul-keeper/
wc -l skills/soul-keeper/SKILL.md
```

Expected: `SKILL.md` exists, < 80 lines.

**Step 3: Commit**

```bash
git add skills/soul-keeper/SKILL.md
git commit -m "feat: add soul-keeper skill — soul update protocol and field guide"
```

---

### Task 5: Integration Verification

**Files:**
- Read: `skills/soul/SOUL.md`
- Read: `skills/soul-keeper/SKILL.md`
- Read: `skills/creating-skills/SKILL.md`
- Read: `skills/creating-skills/templates.md`

**Step 1: Verify all four files exist and are valid**

```bash
ls skills/creating-skills/ skills/soul/ skills/soul-keeper/
```

Expected output:
```
skills/creating-skills/:
SKILL.md  templates.md

skills/soul/:
SOUL.md

skills/soul-keeper/:
SKILL.md
```

**Step 2: Verify YAML frontmatter in all SKILL.md files**

```bash
for f in skills/creating-skills/SKILL.md skills/soul/SOUL.md skills/soul-keeper/SKILL.md; do
  echo "=== $f ==="; head -5 "$f"; echo
done
```

Expected: each file starts with `---`, has `name:` and `description:` fields, description starts with "Use when" (or "Load at" for soul).

**Step 3: Verify the bot picks up all skills on reload**

If the bot is running, trigger a reload via chat: send `reload_skills`. Expected response: `"Skills reloaded. N skill(s) now active."` where N includes `soul`, `soul-keeper`, `creating-skills`.

If testing locally without a running bot:
```bash
cargo check
```

Expected: no compilation errors (this is a skill-only change, so there should be none).

**Step 4: Manual smoke tests (in bot chat)**

Test 1 — Soul embodiment:
- Send: "Hey, what's your name and how do you feel today?"
- Expected: Response in Fox's voice — warm, direct, peer-like, not assistant-like

Test 2 — Soul update:
- Send: "From now on, be more formal and call me by my first name"
- Expected: Agent updates `soul.v`, changes `persona.tone` and `rules`, confirms briefly, then immediately speaks more formally

Test 3 — Creating a skill:
- Send: "Create a skill for summarizing news articles"
- Expected: Agent asks clarifying questions, uses archetype selection, self-evaluates before writing, activates cleanly

---

### Task 6: Push to Branch

**Step 1: Verify git status**

```bash
git status
git log --oneline -6
```

Expected: clean working tree, 4 commits visible (Tasks 1-4).

**Step 2: Push to feature branch**

```bash
git push -u origin claude/agent-skills-personality-OofJq
```

If push fails due to network error, retry with exponential backoff (2s, 4s, 8s, 16s):
```bash
sleep 2 && git push -u origin claude/agent-skills-personality-OofJq
```

**Step 3: Confirm**

```bash
git log --oneline origin/claude/agent-skills-personality-OofJq
```

Expected: all 4 feature commits visible on remote.

---

## Summary

| Task | File | Type |
|------|------|------|
| 1 | `skills/creating-skills/SKILL.md` | Rewrite |
| 2 | `skills/creating-skills/templates.md` | Create |
| 3 | `skills/soul/SOUL.md` | Create |
| 4 | `skills/soul-keeper/SKILL.md` | Create |
| 5 | Integration test | Verify |
| 6 | Push to branch | Deploy |

**Zero Rust changes.** All functionality via existing `write_skill_file`, `reload_skills`, and `SkillRegistry` infrastructure.
