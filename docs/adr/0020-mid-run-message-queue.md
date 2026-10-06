# ADR-0020: Mid-run message queue (steer, never drop)

- **Status:** Accepted
- **Date:** 2026-10-06
- **Branch:** `docs/adr-midrun-and-cron` (decision); implementation in a follow-up PR
- **Decision source:** PO lock 2026-10-06, questions Q4–Q9, plus PO Product GO answers
  2026-10-06 that close all open questions (recorded below)
- **Related:** ADR-0005 (portal one generation per identity), ADR-0013 (typed `RunStop`,
  scheduled runner), design `docs/superpowers/specs/2026-07-08-stop-btw-steer-design.md`

## Context

Product rule: while a user turn (one agent loop) is running, later messages from the same user
must **not be dropped**. They steer the running turn at its next step, or start the next turn.

Recon of `main` @ `318d203` shows the original 2026-07-08 steer design is only half alive:

1. **Steer drain is dead code.** `Agent::queue_injection` (cap 10, text only) and
   `Agent::drain_and_inject_steer` still exist, but the drain lost its two call sites in the
   July `AgenticLoop` extraction (`a401063`). `ConversationManager::apply_steer` has no caller
   either. A message that reaches the queue gets "📨 Steer queued", then is never read. The map
   entry is never cleared, so after 10 such messages the key reports "Injection queue full"
   until restart.
2. **Telegram turns block the chat.** The dispatcher's `distribution_function` serialises
   non-command updates per chat, and `handle_message` awaits `process_message` inline. A
   second message sent during a Telegram turn waits in the dispatcher and later runs as its
   **own separate turn**. It never reaches the `is_processing` branch and never steers.
3. **The queue branch only fires during scheduled runs, and loses the message.** A scheduled
   fire uses the task's `bot_id` + `user_id`, the same `session_key` as the user's chat. So a
   user message during a scheduled run hits branch (1) and is lost.
4. **Scheduled and user turns run in parallel on the same key.** The scheduled job runner (one
   consumer in `main.rs`) does not coordinate with Telegram turns. Both call
   `register_cancel_token` for the same key. The second overwrites the first, and whichever
   finishes first removes the entry. After that, `/stop` cannot reach the other run and
   `is_processing` is wrong.
5. **Media is lost on the queue path.** `queue_injection` stores only `text`, so the
   downloaded photo or document is discarded. Voice and audio are never read at all:
   `handle_message` only looks at `photo()` and `document()`, so a voice note with no caption
   hits "nothing to process" and is silently ignored, even when idle. RustFox has no
   speech-to-text backend today.
6. **Acks open bubbles.** "📨 Steer queued" is a new text message in every mode, including
   `fully_silent` bots.
7. **Portal** rejects a second send during a generation with `409 chat_in_progress`
   (ADR-0005).

Prior work this ADR builds on, but does not re-decide: `/stop` cooperative cancel
(`CancellationToken` checked at each iteration), `/btw` (commands bypass per-chat
serialisation), `/mode steer|queue` (stored per `session_key`, default `steer`), typed
`RunStop` / max-iterations handling (ADR-0013 amendment), and streaming / Thinking placeholder
behaviour. This ADR covers **mid-run ingress only**: what happens to a message that arrives
while a turn for the same key is active.

### PO lock 2026-10-06

| Q | Decision |
|---|----------|
| Q4 Cap | Max **10** queued messages per key. When full, **refuse** the new message with a reply saying too many messages are waiting until this turn finishes. Never drop silently. |
| Q5 `/stop` | Cancel the in-flight turn **and** clear the queue. Reply with how many queued messages were cleared. |
| Q6 Max-iterations | Queued messages not yet injected when a turn stops at max-iterations **start a new user turn automatically**. Never drop. Amendment: auto-start **at most once** (see Decision 7). |
| Q7 Ack | Acknowledging a queued message must **not** open a new bubble. React (👀) in all modes. The final reply answers queued content together. |
| Q8 Schedule | A scheduled run that fires while a user turn is active **waits behind it** (no parallel). Messages sent during a scheduled run are **not** injected into it. After it finishes they start the user's own turn. |
| Q9 Media | Photos, voice and files steer like text. Preprocess (download, voice→text) before inject. Never drop. Amendment: in v1 voice is a placeholder plus an explicit "not supported" reply (see Product GO 3). |

### PO Product GO answers 2026-10-06

These answers close every open question from the first draft of this ADR. None is left
blocking.

1. **Queue-full refusal in `fully_silent` (Q4).** The refusal is a **text reply** in every
   mode, including `fully_silent`. This is required. Silent mode only hides tool bubbles; it
   does not hide "this message was not accepted". A reaction-only refusal (for example 🙅) is
   **rejected**: a refused message must never look accepted.
2. **`/stop` and waiting scheduled runs.** Kept as drafted. `/stop` cancels user-initiated
   work and clears the user queue only. It does **not** skip a scheduled run waiting behind
   the cancelled user turn (Decision 6).
3. **Voice (Q9).** v1 keeps the placeholder item, **and** the bot must send an explicit
   user-facing reply that voice is not supported yet and the user should type. Voice is never
   silently treated as accepted or understood. Local speech-to-text (for example
   whisper.cpp, for privacy) is a **follow-up requirement**, not a v1 choice. There is no
   open question about the speech-to-text backend for v1.
4. **Copy language.** User-facing strings in code are **English** (repo English rule). The
   ADR does not carry non-English product copy. Per-bot language / i18n is backlog. Locked
   English copy:
   - Q6 second max-iterations stop: `This turn stopped at the step limit; you still have
     {N} messages waiting. Send another message and I'll continue.`
   - Voice not supported: `Voice is not supported yet — please type instead.`
5. **Q6 copy superseded.** The Cantonese step-limit copy offered earlier on 2026-10-06 is
   superseded by the English-in-code lock above. The developer uses the English string.

## Decision

1. **One gate per conversation key: `TurnGate`** (new `src/turn_gate.rs`, owned by `Agent`,
   replaces `pending_injections`).
   - Key = existing `session_key(bot_id, user_id)` (`{bot_id}:{user_id}`). This matches
     conversation memory (`platform+bot+user`) and the cancel registry. `chat_id` is stored on
     each queued item for reply routing.
   - Per-key state:
     ```rust
     struct KeyState {
         active: Option<ActiveTurn>,           // kind: User | Scheduled, started_at, cancel token
         pending: VecDeque<QueuedInput>,       // user messages, FIFO, cap 10
         waiting_scheduled: VecDeque<oneshot::Sender<()>>, // runner tickets (Decision 8)
         auto_continue_used: bool,             // Q6 recursion guard
     }
     struct QueuedInput {
         text: String,                // caption/text, or transcript (Decision 9)
         attachments: Vec<Attachment>,
         chat_id: String,
         platform_msg_id: Option<i32>,// Telegram message id (for the reaction)
         temp_dir: Option<PathBuf>,   // owned; removed after injection or clear
         received_at: Instant,
     }
     ```
   - Exactly one turn per key at a time. `register_cancel_token` / `clear_cancel_token` move
     under the gate, so a key has one live token. `is_processing` reads `active.is_some()`.
   - All state changes (`try_begin_user`, `enqueue`, `drain`, `finish`, `clear`) happen under
     one `tokio::sync::Mutex`. `finish` returns what should run next in the same critical
     section, so no message can slip in between "turn ended" and "gate idle".
   - **In-memory only (v1).** A crash, `/restart` or self-upgrade loses queued items. Accepted.
     The only durable queue pattern in the codebase (`pending_reruns`, ADR-0013) stores rows,
     not temp media files, so reusing it is not cheap.

2. **Telegram ingress stops blocking the chat.** `handle_message` keeps doing per-chat ordered
   preprocessing (download, Decision 9), then:
   - Commands (`/stop`, `/btw`, `/mode`, …) are unchanged and never queued.
   - Gate idle → `try_begin_user` → **spawn** the turn (`tokio::spawn(run_user_turn(..))`:
     notifier, streaming, placeholder, send) and return. The dispatcher worker is free, so the
     next message from the same chat reaches the gate.
   - Gate active (user or scheduled turn) → `enqueue` (Decision 3).
   - `/mode queue` keeps its meaning: queued items are not injected mid-run. They start the
     next turn together when the current one ends. Default `steer` injects (Decision 4).

3. **Enqueue, ack, and refuse at the cap (Q4, Q7).**
   - Accepted → `setMessageReaction` with 👀 on the user's message (Bot API 7.0+; 👀 is in
     the standard emoji set bots may use). Same in every tool-UI mode, including
     `fully_silent`. **No text bubble.** If the call fails (for example a group admin limited
     `available_reactions`, or an old client), `warn!` and continue. Do not fail the turn and
     do not fall back to a text bubble.
   - Full (`pending.len() == 10`) → the message is **not** stored, and the bot replies to it:
     `⚠️ Too many messages are waiting (10). Please wait until this turn finishes, then send
     again.` The refusal is a text reply in **every** tool-UI mode, including `fully_silent`
     (Product GO 1). Silent mode hides tool bubbles, not refusals. Do not replace it with a
     reaction. A refused message must not look accepted.
   - The only text replies on the ingress path are this refusal and the voice-not-supported
     notice (Decision 9). Accepted messages get the reaction only.
   - The cap counts items, not bytes. Each photo in a Telegram album is its own update and its
     own item.

4. **Injection point = between loop iterations, never mid-tool (steer).**
   - `AgenticLoop` gets an optional steer source (`Option<Box<dyn SteerSource>>`, with
     `async fn drain(&self) -> Vec<QueuedInput>`). Only the main chat loop passes one.
     Subagents, `spawn_agents`, `invoke_agent` peers and **scheduled runs** pass `None`.
   - Drain point A: at the top of each iteration, right after the cancel check and before
     `messages.prepare(..)`. In practice this is after the previous tool batch finished.
   - Drain point B: before returning `FinalResponse`. If the drain is non-empty, push the
     draft text as an `assistant` message, inject the items, and `continue`. Nothing is
     streamed yet, so the user gets **one final reply** covering the original request and the
     queued content (Q7). This counts against `max_iterations`.
   - Each drained item goes through the same path as a fresh user turn
     (`process_attachments`, then a user-role `ChatMessage`), prefixed `[Steer] ` in steer
     mode. It is **persisted to conversation memory when injected** (today only queue mode
     persists). This keeps history equal to what the model saw.
   - Item `temp_dir`s are removed after the turn that injected them ends.

5. **Turn end: drain leftovers, never drop.** `finish(key, stop)` checks, in this order:
   1. a waiting scheduled ticket exists → hand the gate to it (Decision 8). Leftover `pending`
      waits behind it;
   2. else `pending` is non-empty → start a new user turn whose input is **all** pending items
      in FIFO order (one turn, not one per item), subject to Decision 7 when the previous stop
      was `MaxIterations`;
   3. else → idle.

6. **`/stop` clears turn and queue (Q5).** `/stop` runs `cancel_processing` (loop token and
   supervisor tasks, unchanged), then `TurnGate::clear(key)`, which drops all `pending` items
   (temp files removed) and returns `n`.
   - Something was cancelled → `⏹ Processing cancelled. Cleared {n} queued message(s).`
   - Nothing running but `n > 0` (race at turn end) → `Cleared {n} queued message(s).`
   - Nothing at all → existing `Nothing is currently processing.`
   - `/stop` does **not** discard waiting scheduled tickets. A scheduled run is not a user
     message. It runs next, as it would have. `/stop` only cancels user-initiated work and
     clears the user queue (Product GO 2).

7. **Max-iterations auto-continue, at most once (Q6 + PO amendment 2026-10-06).**
   - A user turn ends with `RunStop::MaxIterations`. The existing max-iterations reply is sent
     as today. Then:
     - `pending` non-empty **and** `auto_continue_used == false` → set
       `auto_continue_used = true` and start **one** new user turn with all pending items
       (after any waiting scheduled run, Decision 5.1).
     - `pending` non-empty **and** `auto_continue_used == true` (the auto-started turn also
       hit max-iterations) → **do not** chain. Keep the queue and reply with the PO-locked
       step-limit message (distinct from the Q4 refusal). Locked English copy (Product GO 4):
       `This turn stopped at the step limit; you still have {N} messages waiting. Send
       another message and I'll continue.` `N` = `pending.len()`. The string lives in code in
       English; no locale layer in v1. Then the gate goes **idle with a non-empty queue**.
   - Idle with a non-empty queue: the user's next non-command message starts a turn with all
     pending items **plus** that message, in arrival order. The cap applies only to messages
     waiting behind an *active* turn, so the triggering message is always accepted.
   - `auto_continue_used` resets to `false` when a turn ends with `FinalResponse` or
     `Cancelled`, and when a user-initiated (non-auto) turn starts.

8. **Scheduled runs wait; never parallel, never steered (Q8).**
   - The job runner calls `TurnGate::begin_scheduled(key).await` before
     `process_message_outcome`. If the key is idle, the run starts at once. Otherwise it gets a
     ticket in `waiting_scheduled` and awaits it. A user turn always finishes before a waiting
     scheduled run starts (Decision 5.1).
   - During a scheduled turn, user messages are enqueued as in Decision 3 (👀 reaction, cap,
     refusal). They are **not** drained into the scheduled loop (no steer source). When the
     scheduled run finishes, `finish` starts the user's own turn with them.
   - The runner stays a single consumer (ADR-0013 serial semantics unchanged). A scheduled run
     waiting on a busy key therefore delays later scheduled runs for other keys
     (head-of-line). Accepted for v1: one owner, a handful of bots, turns of minutes.
   - ADR-0013 re-fires go through the same `begin_scheduled` path.

9. **Media steers like text (Q9).** Preprocessing happens at ingress, before `enqueue` or
   `try_begin_user`. Telegram file links expire, so downloads must not wait for the drain.
   - Photo / document: download as today and keep the `Attachment` on the `QueuedInput`.
     Extraction (vision / OCR / PDF / DOCX via `process_attachments`) runs at injection, the
     same as for a fresh turn.
   - Voice / audio (new): read `msg.voice()` and `msg.audio()`, download, and add
     `AttachmentKind::Audio`. Then call a `transcribe_audio(path) -> Result<String>` hook and
     put the transcript in `text` (prefixed `[Voice] `, caption kept if present).
   - **v1 has no speech-to-text backend (Product GO 3).** The hook returns `Err`, and:
     - the item is still kept (never dropped), with text
       `[Voice message received ({secs}s); not transcribed — content unknown]` plus any
       caption, so the model knows a voice note arrived but must not claim to know what it
       said;
     - the bot **replies to the voice message** at ingress, in every tool-UI mode including
       `fully_silent`: `Voice is not supported yet — please type instead.` This applies
       whether the gate is idle or active. When the voice item is queued it also gets the 👀
       reaction; the reply is what tells the user it was not understood.
     - This also fixes today's idle case, where a voice note with no caption hits "nothing to
       process" and is silently ignored.
   - Local speech-to-text (for example whisper.cpp, run locally for privacy) is a follow-up
     requirement. When it lands, the hook returns `Ok`, and the notice is no longer sent.
   - Download failure → the item is kept with `[Attachment could not be downloaded: {err}]`,
     never silently skipped.

10. **Portal follows the same rules (slice 2, amends ADR-0005).** Portal chat uses the same
    `TurnGate` with its own key (`DEFAULT_BOT_ID`, `user_name`).
    - `POST /api/chat` while busy → `202 {queued: true, position}` instead of
      `409 chat_in_progress`. The item steers the active generation, whose SSE stream carries
      the combined final answer. The UI marks the user bubble "seen" (the 👀 equivalent) and
      opens no assistant bubble.
    - Full → `409 queue_full` with the Q4 text.
    - `POST /api/chat/cancel` clears the queue and returns `{cleared: n}`.
    - Ship Telegram (slice 1) first. Slice 2 can be its own PR.

## Rejected alternatives

- **Keep per-chat blocking and just wire the old drain back in.** It would only ever steer
  scheduled runs, which Q8 forbids. Normal chat messages would still never steer.
- **One new turn per queued message.** Fragments the conversation and multiplies LLM cost. Q7
  wants one reply covering the queued content.
- **Inject mid-tool (abort the running tool).** Tools have side effects (shell, posting).
  Steering at the iteration boundary is enough and is what the 2026-07-08 design intended.
- **Durable SQLite queue for v1.** Queued media lives in temp dirs, and crash loss of a few
  in-flight chat messages is acceptable for a personal assistant. Revisit if restarts become
  frequent.
- **Unlimited auto-continue after max-iterations.** A task that keeps exhausting its budget
  would loop with no human in it, burning tokens. One auto turn, then ask (PO amendment).
- **Reaction-only refusal in `fully_silent` (for example 🙅).** A reaction is too easy to
  miss, and a refused message must not look accepted (Product GO 1).
- **Silently injecting voice as if it were understood.** The user would think the bot heard
  them (Product GO 3).
- **Non-English product copy in code for v1.** The repo rule is English. Per-bot language is
  backlog (Product GO 4, 5).
- **Ack with a text bubble in non-silent modes.** Q7 says reaction in all modes. Bubbles clog
  the chat exactly when the user is sending several messages.
- **Parallel scheduled and user turns on the same key.** Two loops writing to one conversation
  and one cancel slot. This is the current bug (Context 4).

## Consequences

- ✅ No mid-run message is silently lost. Every message is injected, starts a turn, or is
  refused with a reply. The only loss is a crash or restart (documented).
- ✅ `/stop` and `is_processing` become reliable: one live turn and one token per key.
- ✅ Fixes today's stuck "Injection queue full" state (dead drain plus a map that never
  clears).
- ⚠️ Telegram handler control flow changes (turn spawned, handler returns). Message ordering
  is now kept by the gate's FIFO instead of dispatcher blocking. Tests must cover ordering.
- ⚠️ Scheduled head-of-line blocking (Decision 8) can delay unrelated scheduled tasks while a
  long user turn runs.
- ⚠️ Without a speech-to-text backend, voice steers only as a placeholder, and the user is
  told to type instead. Local speech-to-text is a follow-up requirement (see Follow-ups).
- ⚠️ Drain point B adds one LLM round when messages arrive during final-answer generation. It
  counts toward `max_iterations`.
- ⚠️ Group chats: the key is per bot+user, so the same user in two groups with the same bot
  shares one queue. Rare for this product. Revisit by adding `chat_id` to the key if needed.

## Tests

Pure `TurnGate` unit tests (no Telegram), plus `AgenticLoop` tests with a fake LLM and a fake
`SteerSource`:

- **Cap + reject:** 10 enqueues accepted; the 11th returns `Full`, is not stored, and the
  handler produces the refusal text; a reaction is requested only for accepted items.
- **Refusal in `fully_silent`:** with tool UI mode `fully_silent`, the 11th message still
  gets the refusal **text** reply (not a reaction only).
- **Reaction failure:** `setMessageReaction` error → logged, enqueue still `Accepted`, no
  text message sent.
- **Steer injection:** an item enqueued during iteration k is in the messages of LLM call k+1
  (drain point A), never between two tool calls of one batch, and is persisted to memory.
- **Drain point B:** an item enqueued while the fake LLM returns final text → loop continues,
  one final reply, the draft is not streamed.
- **Cancel clears N:** 3 pending + active turn → `/stop` reply reports 3; `pending` empty;
  temp dirs removed; waiting scheduled ticket still present.
- **Max-iter drains to new turn:** turn ends `MaxIterations` with 2 pending → exactly one auto
  turn starts with both items in FIFO order; `auto_continue_used == true`.
- **Recursion guard:** the auto turn also hits `MaxIterations` with N pending → no third turn;
  the step-limit message (with N, distinct from the Q4 text) is produced; the gate is idle
  with the queue kept; the next user message starts a turn with pending + new message; the
  guard resets on `FinalResponse`.
- **Schedule serialisation:** `begin_scheduled` on an active user key blocks until `finish`;
  never two active turns for one key; a user message during a scheduled turn is not drained
  into it and starts a user turn after it ends; waiting scheduled goes before leftover
  pending.
- **Media after preprocess:** photo/document items keep their `Attachment` and go through
  `process_attachments` at injection; a voice item carries the transcript (fake hook `Ok`) or
  the placeholder (hook `Err`); a download failure yields a placeholder item, never a dropped
  one.
- **Voice not supported (v1):** with the hook returning `Err`, a voice message (idle and
  while a turn is active, and in `fully_silent`) produces the reply `Voice is not supported
  yet — please type instead.`; the item is kept with the "not transcribed" placeholder; an
  idle caption-less voice note is no longer ignored.
- **Step-limit copy:** the second max-iterations stop produces the locked English string
  with the correct `N`.
- **No-slip finish:** concurrent `enqueue` racing `finish` → the item either starts the next
  turn or was drained, never lost (loop the race in a test with `tokio::task::yield_now`).
- **Portal (slice 2):** busy `POST /api/chat` → 202 + position; 11th → `409 queue_full`;
  cancel returns `cleared`.

## Follow-ups (not blocking)

All open questions are resolved by the PO Product GO answers (2026-10-06). Remaining
follow-ups, each its own issue:

1. **Local speech-to-text** (for example whisper.cpp) behind `transcribe_audio`, for
   privacy. Required, after v1.
2. **Per-bot language / i18n** for Telegram replies. Backlog. Until then, user-facing strings
   in code stay English.
