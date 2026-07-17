# ADR 0001: Supervisor Module Structure

## Status
Accepted

## Date
2026-07-15

## Context
The Supervisor module has 17 source files (`task.rs`, `job.rs`, `state.rs`, etc.)
with only one caller (the Supervisor facade). Should they be consolidated?

## Decision
Keep the 17-file split. Each file represents one stage of the pipeline.

## Rationale
- Each file is small (50–150 lines) and focused on one responsibility
- New pipeline stages can be added without modifying existing files
- The `backend/` submodule already justifies the structure (6 backend types)
- The design anticipates future variations

## Consequences
- Higher file count but each file is easier to navigate
- If after 6 months no second variation exists, consolidate
- Adding a new backend requires only a new file + Registry registration