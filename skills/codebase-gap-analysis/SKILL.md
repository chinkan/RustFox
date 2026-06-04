---
name: codebase-gap-analysis
description: 'Analyze a user''s codebase against a reference architecture to identify gaps and provide prioritized enhancement recommendations.'
tags: ['research', 'architecture', 'codebase', 'comparison', 'gap-analysis', 'project-review']
---

# Codebase Gap Analysis vs Reference Architecture

Use when the user wants to deeply compare their project or codebase against a reference implementation, framework, or architectural pattern to find gaps and improvement opportunities.

## Workflow

### 1. Define Scope & Dimensions
- Confirm the user's project location (repo URL, local path, or pasted code).
- Confirm the reference target (technology, product, or architectural pattern).
- Select comparison dimensions relevant to the domain (e.g., orchestration, memory, concurrency, skills, backends, observability, agent lifecycle).

### 2. Explore User Codebase
- Fetch repository metadata and directory structure.
- Read root configuration and documentation files.
- Dive into core source directories to map the architecture.
- Identify and read critical modules (planner, orchestrator, supervisor, memory, tools, agents, config).
- Summarize current capabilities, abstractions, and design patterns found.

### 3. Research Reference Architecture
- Search official documentation, repositories, and technical articles for the reference system.
- Extract concrete capabilities, configuration mechanisms, and architectural layers.
- Note any experimental, undocumented, or advanced features relevant to the comparison.

### 4. Execute Structured Comparison
- Build a side-by-side comparison table across the chosen dimensions.
- Flag capabilities present in the reference but missing or incomplete in the user's project.
- Note areas where the user's project may be ahead, equivalent, or intentionally divergent.

### 5. Prioritized Recommendations
- List each gap with: description, impact, and suggested implementation approach aligned with the user's stack.
- Assign priority (Critical / High / Medium / Low).
- Propose a pragmatic roadmap that respects existing dependencies and build patterns.

### 6. Deliver Synthesis
- Provide an overall architectural maturity assessment.
- Highlight quick wins versus long-term investments.
- Offer to elaborate on, design, or implement any specific recommendation.
