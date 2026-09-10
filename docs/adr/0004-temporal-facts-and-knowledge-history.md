# Temporal Facts and Knowledge History

Memory gains two layers: Knowledge History (auto-archive on `remember`/`forget` via SQLite triggers) and Facts (explicit bi-temporal triples). Knowledge stays the live KV snapshot; Facts carry validity windows. Schema uses `entity`/`relation`/`value`/`valid_from`/`valid_to` (not subject/object_value/valid_until). One active Fact per `(entity, relation)`; new value auto-closes the prior active row. No agent-loop changes in this phase — data layer + MCP tools only. Agentic RAG and harness evolution deferred to separate issues.
