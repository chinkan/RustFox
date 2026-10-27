# ADR-0048: Telegram inbound Reply-to → Active

- **Status:** Proposed (awaiting Product Accept)
- **Date:** 2026-10-27
- **Related:** ADR-0020 (mid-run message queue / TurnGate); Continuity #279 / #282 / #285 — **do not change**
- **Decision source:** PO grill 2026-10-27

## Context

On Telegram, a user can send a message that **replies to** an earlier message (`reply_to_message`). Today that quote context is not injected into the Active turn's `messages[]`, so the model may not see what the user is answering. Operators need a clear product lock that:

- puts the quoted prior message into **this turn's Active `messages[]`**, and
- is **not** Continuity (SQLite → history SoT / anti cold-intro; #279 / #282 / #285),
- is **not** a Telegram emoji Reaction on the inbound message,
- is **not** Steer / Mid-run queue semantics (ADR-0020), though mid-run ingress still uses TurnGate.

This document records the v1 product locks for Telegram inbound reply-to → Active. It does **not** implement Rust, tests, runtime, or a release tag. Soft test is N/A for this docs-only PR.

## Decision

Product locks (PO grill 2026-10-27) are authoritative for the first implementable slice after this ADR is Accepted and the usual four gates pass:

1. **v1 Must — Telegram `reply_to_message` → Active.** When an inbound Telegram message carries `reply_to_message`, inject the quote into **this turn's** Active `messages[]`.

2. **Quote payload.** Carry quoted text or caption, the replied-to `message_id`, and role `bot` | `user`. Media-only quotes use the media placeholder (Decision 10).

3. **Reaction out of scope.** Handling inbound Telegram emoji reactions is a **follow-up**, not v1.

4. **Telegram only for v1.** Portal and other channels are out of scope for the first slice.

5. **Not Continuity.** Do **not** change Continuity SoT, anti cold-intro, or matcher work (#279 / #282 / #285). Inbound reply-to is quote injection for the current turn, not history rewrite.

6. **Persist composed user text.** Persist the **full composed** user text (template below) into existing SQLite conversation history. **No new schema.**

7. **One user message — locked English template** (exact):

```
[Reply to {role} msg {id}]:
{quoted_text}

{user_text}
```

Compose a **single** user-role message for the turn (not a separate system/assistant quote row).

8. **Mid-run — ADR-0020 TurnGate.** If the reply arrives while a turn is active, ADR-0020 TurnGate applies. `QueuedInput` **keeps the quote** (composed text includes the reply-to prefix) through enqueue / steer / next-turn drain. Outbound 👀 ack remains **only** the ADR-0020 mid-run ack (Decision 12).

9. **Unavailable quote — no silent degrade.** If the replied-to body cannot be resolved, still inject:

```
[Reply to {role} msg {id}: unavailable]
```

plus known `role` / `id`. Do **not** omit the reply-to marker, and do **not** refuse the user message.

10. **Media-only placeholder** (exact):

```
[Reply to {role} msg {id}: media attachment, no text]
```

11. **Quote truncate.** Truncate `{quoted_text}` to **4000 Unicode scalars**, then append `…` when truncated.

12. **Outbound 👀.** Mid-run acknowledgement stays the ADR-0020 👀 reaction on the **user's new message**. Reply-to does not invent a second ack path.

## Non-goals

- Continuity rewrite (#279 / #282 / #285) — leave unchanged.
- Inbound Telegram emoji **Reaction** handling (follow-up).
- Portal or other non-Telegram channels in v1.
- Any Rust / tests / runtime / `package.json` / `Cargo.lock` change in this docs PR.
- Soft test or a release tag from this docs PR.
- Changing ADR-0020 TurnGate / Steer / Mid-run queue semantics beyond requiring `QueuedInput` to preserve the composed quote.

## Rejected alternatives

- **Treat reply-to as Continuity.** Continuity is SQLite → `messages[]` history / anti cold-intro. Reply-to is a per-turn quote prefix on the inbound user text. Mixing them would couple unrelated tracks (#279 / #282 / #285).
- **Separate system or assistant message for the quote.** Product lock is **one** composed user message with the locked template.
- **Silent omit when quote unavailable.** Operators must always see a reply-to marker (`unavailable` or media placeholder); silent degrade and refuse are both rejected.
- **Ship Reaction handling in v1.** Out of scope; follow-up.
- **Portal / multi-channel in v1.** Telegram only for the first slice.
- **New SQLite schema for quotes.** Persist the composed text in existing history rows only.
- **A second outbound ack for reply-to.** 👀 remains ADR-0020 mid-run ack only.

## Consequences

- Telegram reply-to becomes visible to Active for the current turn via a single composed user message.
- Continuity, Reaction, Steer, and Mid-run queue stay distinct terms (see `GLOSSARY.md`).
- Mid-run replies keep their quote through TurnGate / `QueuedInput` without a parallel queue.
- Unavailable and media-only quotes remain explicit in history; no silent loss of the reply-to signal.
- Long quotes are bounded (4000 Unicode scalars + `…`) before persist and model input.
- Implementation waits on Product Accept → status flip to Accepted, then the usual four gates on an impl PR. Soft N/A on this docs PR; no tag.

## Tests

Docs-level acceptance checklist for a **later** implementation PR (not implemented here):

- [ ] Compose uses the locked English template exactly (one user message; `{role}`, `{id}`, `{quoted_text}`, `{user_text}`).
- [ ] Quote truncate: >4000 Unicode scalars → cut to 4000 + `…`.
- [ ] Unavailable: inject `[Reply to {role} msg {id}: unavailable]` with known role/id; message still accepted (no silent omit, no refuse).
- [ ] Media-only: inject `[Reply to {role} msg {id}: media attachment, no text]`.
- [ ] Mid-run: TurnGate enqueue / steer / next-turn drain preserves the composed quote on `QueuedInput`.
- [ ] SQLite history stores the **full composed** user text; no new schema.
- [ ] No Continuity coupling: Continuity SoT / anti cold-intro / matcher paths unchanged.
- [ ] Outbound 👀 still only ADR-0020 mid-run ack on the new user message.

## Follow-ups

1. **Reaction** — inbound Telegram emoji reaction handling (out of v1).
2. **Non-Telegram channels** — portal / other ingress once Telegram v1 is proven.
3. **Implementation PR** — after Product Accept, flip this ADR to **Accepted**, then ship runtime behind the usual **four gates**. Soft QA only after impl; **no tag** from the docs PR.

## Related

- ADR-0020 — Mid-run message queue / TurnGate / Steer / 👀 ack; `QueuedInput` must keep the composed quote.
- Continuity #279 / #282 / #285 — SQLite → `messages[]` SoT and anti cold-intro; **do not change** under this ADR.
- `GLOSSARY.md` — **Quoted reply** / **Inbound reply-to** vs Continuity / Reaction / Steer / Mid-run queue.
- PO grill 2026-10-27 — product locks recorded above; Status remains Proposed until Product Accept.
