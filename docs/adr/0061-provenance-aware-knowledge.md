# ADR-0061: Provenance-aware knowledge; unknown is not trusted

- **Status:** Accepted
- **Date:** 2026-11-07
- **Issue:** #336
- **Product lock:** [comment 6595630527](https://github.com/chinkan/RustFox/issues/336#issuecomment-6595630527) (2026-11-03, recorded at `1e4adf7`). This implementation is a tip on `1e4adf7`.
- **Does not block:** no version bump. No tag.

## Context

`knowledge` had a nullable `source` and nothing else. Real memories were stored with `source` NULL. Dream-cycle can merge, rewrite, and delete those rows, and `knowledge_history` only records the old and new text. After a reset there is no pointer back to the run, message, file, or URL the line came from. CORAL (`arXiv:2610.40350` §D.2) names that as a deployment risk: a fact with no evidence cannot be re-checked, and a low-quality write propagates with no record of where it came from.

Product locked options A+B+C. Option D (dream merge, file processor, portal, instance snapshot) is in this tip where the code lives in the repo.

## Decision

1. **`knowledge.evidence` is TEXT.** The value is one JSON object `{kind, ref, captured_at}` or an array of those objects. `kind` is `run`, `conversation`, `file`, `url`, `dream`, or `unknown`. `ref` and `captured_at` are omitted when unknown. The existing `source` column stays.

2. **Migration backfill is honest.** Rows with empty evidence become `{"kind":"unknown"}`. A `source` that looks like a filename (dotted basename, short extension with a letter, not a URL) becomes `{"kind":"file","ref":"<that source>"}` with no `captured_at`. Prose, versions, and URLs stay `unknown`. The only new facts written are that kind and, for a filename, the source string itself. A later startup leaves a non-empty `evidence` value alone.

3. **`remember` takes an optional `evidence` argument.** Omitted, or JSON null, stores `unknown`. That is untrusted. It is not "keep the previous pointer": a later `remember` of the same category and key replaces `evidence`. A non-`unknown` kind requires a non-empty `ref`. An invalid kind is rejected and the row is not written. The tool passes `source` as omitted, and the upsert leaves an existing `source` in place (`COALESCE`). A new `source` still replaces the old one.

4. **`recall` and `search_memory` show evidence on knowledge hits.** A trusted pointer is rendered as `evidence: ` plus the JSON (unknown siblings in an array are left out of that line). Unknown, empty, or unreadable evidence is `⚠️ 無來源`. Conversation hits in `search_memory` are unchanged. `MemoryStore::recall` still returns the raw value so settings keys stay parseable.

5. **In-repo writers and the portal.** `file_processor` stores `kind=file`, `ref` = the filename it has, `captured_at` = index time. `GET /api/memory/search` adds `evidence` and `evidenceText` on knowledge rows. The Memory page shows `evidenceText`.

6. **Out of this decision.** No automatic injection of the current run or conversation id into tool context. `knowledge_history` is not extended into a "why" audit. #331, #330, and #333 stay closed. No tag.

## Follow-up (instance, not this repo)

`skills/dream-cycle` §D1 and `scripts/memory-snapshot.sh` are instance files. They are not in this tree, so this tip does not edit them.

- Dream merge: pass the union of the source rows' evidence and add `{kind:"dream", ref:"<source knowledge id and/or dream file>"}`. `memory-manager` states that rule. The instance skill still needs the same paragraph.
- A SQLite dump of `knowledge` includes `evidence` after migration. An instance snapshot that lists columns by name needs `evidence` added.

## Consequences

- The 34 memories that had `source` NULL stay permanently `unknown`. Displaying the warning is the point.
- A model that calls `remember` without `evidence` marks that row untrusted, including when it updates a row that previously had a file pointer. `source` (the filename) remains for a later backfill-style read, but the shown provenance follows `evidence`.
- Settings rows written through `remember` are `unknown`. Their raw value is what the settings readers use.

## Rejected

- Treating a missing pointer as trusted, or copying `source` into `evidence` on every new write.
- Guessing `kind=url` from a `source` string during backfill.
- Auto-filling run or conversation ids inside `ToolContext` (explicit follow-up, not this lock).
- Folding `knowledge_history` into the shipped "why did this change" audit.
