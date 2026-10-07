//! ADR-0020: one gate per conversation key (`session_key(bot_id, user_id)`).
//!
//! At most one turn runs per key. Messages that arrive while a turn is active
//! wait in a FIFO (cap [`QUEUE_CAP`]); they steer the running user turn at its
//! next step, or start the next turn when it ends. Nothing is dropped silently:
//! every message is started, queued, or refused. In-memory only (v1).

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;

use tokio::sync::Mutex;

use crate::agent::RunStop;
use crate::platform::Attachment;

/// Max messages waiting behind an active turn (PO Q4).
pub const QUEUE_CAP: usize = 10;

/// Q4 refusal. A text reply in every tool-UI mode, including `fully_silent`.
pub const QUEUE_FULL_TEXT: &str =
    "⚠️ Too many messages are waiting (10). Please wait until this turn finishes, then send again.";

/// Q6: the auto-started turn also hit the step limit (PO-locked English copy).
pub fn step_limit_text(n: usize) -> String {
    format!(
        "This turn stopped at the step limit; you still have {n} messages waiting. \
         Send another message and I'll continue."
    )
}

/// One user message, already preprocessed (media downloaded) at ingress.
#[derive(Debug, Clone, Default)]
pub struct QueuedInput {
    pub text: String,
    pub attachments: Vec<Attachment>,
    pub chat_id: String,
    pub user_name: String,
    /// Telegram message id (for the 👀 reaction / reply).
    pub platform_msg_id: Option<i32>,
    /// Owned download dir; removed once the input has been consumed or cleared.
    pub temp_dir: Option<PathBuf>,
}

impl QueuedInput {
    /// Remove the owned temp dir (best effort).
    pub async fn cleanup(&self) {
        if let Some(dir) = &self.temp_dir {
            tokio::fs::remove_dir_all(dir).await.ok();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnKind {
    User,
    Scheduled,
}

/// Result of [`TurnGate::submit`].
#[derive(Debug)]
pub enum Submit {
    /// The key was idle: a user turn is now active. Run it with these inputs
    /// (any leftover queue first, then the new message — arrival order).
    Start(Vec<QueuedInput>),
    /// Queued behind the active turn (👀).
    Accepted,
    /// Queue full: not stored (Q4 refusal). The input is handed back.
    Full(QueuedInput),
}

/// What runs after a turn ends ([`TurnGate::finish`]).
#[derive(Debug)]
pub enum Next {
    Idle,
    /// A waiting scheduled run now owns the key (its ticket was released).
    Scheduled,
    /// A new user turn is already active; run it with these inputs.
    User {
        inputs: Vec<QueuedInput>,
        auto_continue: bool,
    },
    /// Q6 recursion guard: the auto turn hit the step limit again. The queue
    /// is kept and the gate is idle; tell the user `n` messages are waiting.
    StepLimit {
        n: usize,
    },
}

#[derive(Default)]
struct KeyState {
    active: Option<TurnKind>,
    pending: VecDeque<QueuedInput>,
    waiting_scheduled: VecDeque<tokio::sync::oneshot::Sender<()>>,
    auto_continue_used: bool,
}

/// All state changes happen under one mutex, so nothing can slip in between
/// "turn ended" and "gate idle".
#[derive(Default)]
pub struct TurnGate {
    keys: Mutex<HashMap<String, KeyState>>,
}

impl TurnGate {
    pub fn new() -> Self {
        Self::default()
    }

    /// A user message arrives (non-command, preprocessed).
    pub async fn submit(&self, key: &str, input: QueuedInput) -> Submit {
        let mut keys = self.keys.lock().await;
        let st = keys.entry(key.to_string()).or_default();
        if st.active.is_none() {
            st.active = Some(TurnKind::User);
            st.auto_continue_used = false;
            let mut inputs: Vec<_> = st.pending.drain(..).collect();
            inputs.push(input);
            return Submit::Start(inputs);
        }
        if st.pending.len() >= QUEUE_CAP {
            return Submit::Full(input);
        }
        st.pending.push_back(input);
        Submit::Accepted
    }

    /// ADR-0020 Decision 8: claim the key for a scheduled run.
    ///
    /// Idle → mark `Scheduled` and return at once. Busy → enqueue a oneshot
    /// ticket in `waiting_scheduled` and await until [`finish`] hands us the
    /// gate (user turn always finishes first).
    pub async fn begin_scheduled(&self, key: &str) {
        loop {
            let rx = {
                let mut keys = self.keys.lock().await;
                let st = keys.entry(key.to_string()).or_default();
                if st.active.is_none() {
                    st.active = Some(TurnKind::Scheduled);
                    return;
                }
                let (tx, rx) = tokio::sync::oneshot::channel();
                st.waiting_scheduled.push_back(tx);
                rx
            };
            // finish already set `active = Scheduled` before sending.
            if rx.await.is_ok() {
                return;
            }
            // Sender dropped without handoff — retry.
        }
    }

    /// Steer drain: queued inputs for the active **user** turn. A scheduled
    /// turn is never steered (Q8), so this returns nothing during one.
    pub async fn drain(&self, key: &str) -> Vec<QueuedInput> {
        let mut keys = self.keys.lock().await;
        match keys.get_mut(key) {
            Some(st) if st.active == Some(TurnKind::User) => st.pending.drain(..).collect(),
            _ => Vec::new(),
        }
    }

    /// The active turn ended with `stop` (`None` = the turn errored).
    /// Returns what runs next, already marked active (ADR-0020 Decision 5/7).
    pub async fn finish(&self, key: &str, stop: Option<RunStop>) -> Next {
        let mut keys = self.keys.lock().await;
        let st = keys.entry(key.to_string()).or_default();
        let ended = st.active.take();
        let user_max_iter = ended == Some(TurnKind::User) && stop == Some(RunStop::MaxIterations);
        if matches!(stop, Some(RunStop::FinalResponse | RunStop::Cancelled)) {
            st.auto_continue_used = false;
        }

        // 1. A waiting scheduled run goes first; leftovers wait behind it.
        while let Some(ticket) = st.waiting_scheduled.pop_front() {
            if ticket.send(()).is_ok() {
                st.active = Some(TurnKind::Scheduled);
                return Next::Scheduled;
            }
        }
        // 2. Leftover messages start one user turn (never dropped).
        if st.pending.is_empty() {
            return Next::Idle;
        }
        if user_max_iter && st.auto_continue_used {
            return Next::StepLimit {
                n: st.pending.len(),
            };
        }
        st.auto_continue_used = user_max_iter;
        st.active = Some(TurnKind::User);
        Next::User {
            inputs: st.pending.drain(..).collect(),
            auto_continue: user_max_iter,
        }
    }

    /// `/stop`: drop every queued message (temp files removed). Waiting
    /// scheduled runs are kept (PO GO 2). Returns how many were cleared.
    pub async fn clear(&self, key: &str) -> usize {
        let cleared: Vec<_> = {
            let mut keys = self.keys.lock().await;
            match keys.get_mut(key) {
                Some(st) => st.pending.drain(..).collect(),
                None => Vec::new(),
            }
        };
        for item in &cleared {
            item.cleanup().await;
        }
        cleared.len()
    }

    pub async fn is_active(&self, key: &str) -> bool {
        self.keys
            .lock()
            .await
            .get(key)
            .is_some_and(|s| s.active.is_some())
    }

    pub async fn pending_len(&self, key: &str) -> usize {
        self.keys
            .lock()
            .await
            .get(key)
            .map_or(0, |s| s.pending.len())
    }

    /// Put Start leftovers back on the pending queue so a Steer drain (or
    /// finish) can consume them in arrival order. Cap is not re-checked:
    /// these items were already accepted.
    pub async fn restore_pending(&self, key: &str, items: Vec<QueuedInput>) {
        if items.is_empty() {
            return;
        }
        let mut keys = self.keys.lock().await;
        let st = keys.entry(key.to_string()).or_default();
        for item in items.into_iter().rev() {
            st.pending.push_front(item);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(t: &str) -> QueuedInput {
        QueuedInput {
            text: t.into(),
            ..Default::default()
        }
    }

    fn texts(v: &[QueuedInput]) -> Vec<&str> {
        v.iter().map(|i| i.text.as_str()).collect()
    }

    #[tokio::test]
    async fn idle_starts_then_queues_up_to_cap_then_refuses() {
        let g = TurnGate::new();
        assert!(matches!(g.submit("k", input("a")).await, Submit::Start(v) if texts(&v) == ["a"]));
        for i in 0..QUEUE_CAP {
            assert!(matches!(
                g.submit("k", input(&i.to_string())).await,
                Submit::Accepted
            ));
        }
        match g.submit("k", input("11th")).await {
            Submit::Full(back) => assert_eq!(back.text, "11th"),
            other => panic!("expected Full, got {other:?}"),
        }
        assert_eq!(
            g.pending_len("k").await,
            QUEUE_CAP,
            "refused item not stored"
        );
    }

    #[tokio::test]
    async fn drain_returns_fifo_for_user_turn_only() {
        let g = TurnGate::new();
        g.submit("k", input("start")).await;
        g.submit("k", input("1")).await;
        g.submit("k", input("2")).await;
        assert_eq!(texts(&g.drain("k").await), ["1", "2"]);
        assert!(g.drain("k").await.is_empty());
    }

    #[tokio::test]
    async fn finish_starts_one_turn_with_all_leftovers_then_idles() {
        let g = TurnGate::new();
        g.submit("k", input("start")).await;
        g.submit("k", input("1")).await;
        g.submit("k", input("2")).await;
        match g.finish("k", Some(RunStop::FinalResponse)).await {
            Next::User {
                inputs,
                auto_continue,
            } => {
                assert_eq!(texts(&inputs), ["1", "2"]);
                assert!(!auto_continue);
            }
            other => panic!("{other:?}"),
        }
        assert!(g.is_active("k").await);
        assert!(matches!(
            g.finish("k", Some(RunStop::FinalResponse)).await,
            Next::Idle
        ));
        assert!(!g.is_active("k").await);
    }

    #[tokio::test]
    async fn errored_turn_still_runs_leftovers() {
        let g = TurnGate::new();
        g.submit("k", input("start")).await;
        g.submit("k", input("1")).await;
        assert!(matches!(g.finish("k", None).await, Next::User { .. }));
    }

    #[tokio::test]
    async fn max_iterations_auto_continues_once_then_step_limit() {
        let g = TurnGate::new();
        g.submit("k", input("start")).await;
        g.submit("k", input("1")).await;
        g.submit("k", input("2")).await;
        match g.finish("k", Some(RunStop::MaxIterations)).await {
            Next::User {
                inputs,
                auto_continue,
            } => {
                assert_eq!(texts(&inputs), ["1", "2"]);
                assert!(auto_continue);
            }
            other => panic!("{other:?}"),
        }
        // The auto turn also stops at the limit with 3 waiting → no third turn.
        for t in ["3", "4", "5"] {
            g.submit("k", input(t)).await;
        }
        match g.finish("k", Some(RunStop::MaxIterations)).await {
            Next::StepLimit { n } => assert_eq!(n, 3),
            other => panic!("{other:?}"),
        }
        assert!(!g.is_active("k").await, "idle with the queue kept");
        assert_eq!(g.pending_len("k").await, 3);
        assert!(step_limit_text(3).contains("you still have 3 messages waiting"));
        assert_ne!(step_limit_text(3), QUEUE_FULL_TEXT);

        // Next user message: pending + new, arrival order; guard reset.
        match g.submit("k", input("6")).await {
            Submit::Start(v) => assert_eq!(texts(&v), ["3", "4", "5", "6"]),
            other => panic!("{other:?}"),
        }
        g.submit("k", input("7")).await;
        assert!(matches!(
            g.finish("k", Some(RunStop::MaxIterations)).await,
            Next::User {
                auto_continue: true,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn final_response_resets_the_guard() {
        let g = TurnGate::new();
        g.submit("k", input("start")).await;
        g.submit("k", input("1")).await;
        g.finish("k", Some(RunStop::MaxIterations)).await; // auto turn
        g.submit("k", input("2")).await;
        // auto turn answers normally → its leftovers start a plain turn
        assert!(matches!(
            g.finish("k", Some(RunStop::FinalResponse)).await,
            Next::User {
                auto_continue: false,
                ..
            }
        ));
        g.submit("k", input("3")).await;
        assert!(matches!(
            g.finish("k", Some(RunStop::MaxIterations)).await,
            Next::User {
                auto_continue: true,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn clear_drops_queue_and_removes_temp_dirs() {
        let g = TurnGate::new();
        let dir = tempfile::tempdir().unwrap().keep();
        g.submit("k", input("start")).await;
        for t in ["1", "2"] {
            g.submit("k", input(t)).await;
        }
        g.submit(
            "k",
            QueuedInput {
                text: "3".into(),
                temp_dir: Some(dir.clone()),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(g.clear("k").await, 3);
        assert_eq!(g.pending_len("k").await, 0);
        assert!(!dir.exists());
        assert_eq!(g.clear("other").await, 0);
    }

    #[tokio::test]
    async fn keys_are_isolated() {
        let g = TurnGate::new();
        assert!(matches!(g.submit("a", input("x")).await, Submit::Start(_)));
        assert!(matches!(g.submit("b", input("y")).await, Submit::Start(_)));
    }

    /// No-slip: an enqueue racing `finish` is either drained into the next
    /// turn or starts its own — never lost.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn enqueue_racing_finish_is_never_lost() {
        for round in 0..200 {
            let g = std::sync::Arc::new(TurnGate::new());
            g.submit("k", input("start")).await;
            let g2 = g.clone();
            let sub = tokio::spawn(async move {
                tokio::task::yield_now().await;
                g2.submit("k", input("late")).await
            });
            let fin = g.finish("k", Some(RunStop::FinalResponse)).await;
            let sub = sub.await.unwrap();
            let seen = match (&fin, &sub) {
                (Next::User { inputs, .. }, Submit::Accepted) => texts(inputs) == ["late"],
                (Next::Idle, Submit::Start(v)) => texts(v) == ["late"],
                _ => false,
            };
            assert!(seen, "round {round}: lost message ({fin:?}, {sub:?})");
        }
    }

    #[tokio::test]
    async fn begin_scheduled_on_idle_marks_active() {
        let g = TurnGate::new();
        g.begin_scheduled("k").await;
        assert!(g.is_active("k").await);
        assert!(matches!(
            g.finish("k", Some(RunStop::FinalResponse)).await,
            Next::Idle
        ));
    }

    #[tokio::test]
    async fn begin_scheduled_waits_until_user_finish_never_two_active() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        use std::time::Duration;

        let g = Arc::new(TurnGate::new());
        assert!(matches!(
            g.submit("k", input("start")).await,
            Submit::Start(_)
        ));

        let started = Arc::new(AtomicBool::new(false));
        let g2 = g.clone();
        let started2 = started.clone();
        let waiter = tokio::spawn(async move {
            g2.begin_scheduled("k").await;
            started2.store(true, Ordering::SeqCst);
        });

        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(
            !started.load(Ordering::SeqCst),
            "scheduled must wait behind active user"
        );
        // User message while waiting still queues (gate busy).
        assert!(matches!(
            g.submit("k", input("mid")).await,
            Submit::Accepted
        ));
        assert_eq!(g.pending_len("k").await, 1);

        match g.finish("k", Some(RunStop::FinalResponse)).await {
            Next::Scheduled => {}
            other => panic!("expected Scheduled handoff, got {other:?}"),
        }
        waiter.await.unwrap();
        assert!(started.load(Ordering::SeqCst));
        assert!(g.is_active("k").await, "scheduled owns the key");
        // Leftover pending waits behind the scheduled turn (Decision 5.1).
        assert_eq!(g.pending_len("k").await, 1);
        // Scheduled is never steered.
        assert!(g.drain("k").await.is_empty());

        match g.finish("k", Some(RunStop::FinalResponse)).await {
            Next::User { inputs, .. } => assert_eq!(texts(&inputs), ["mid"]),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn user_message_during_scheduled_is_not_drained() {
        let g = TurnGate::new();
        g.begin_scheduled("k").await;
        assert!(matches!(g.submit("k", input("hi")).await, Submit::Accepted));
        assert!(
            g.drain("k").await.is_empty(),
            "scheduled turns have no steer drain"
        );
        assert_eq!(g.pending_len("k").await, 1);
        match g.finish("k", Some(RunStop::FinalResponse)).await {
            Next::User { inputs, .. } => assert_eq!(texts(&inputs), ["hi"]),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn waiting_scheduled_before_leftover_pending() {
        let g = TurnGate::new();
        g.submit("k", input("start")).await;
        g.submit("k", input("left")).await;

        let g2 = std::sync::Arc::new(g);
        let g3 = g2.clone();
        let waiter = tokio::spawn(async move {
            g3.begin_scheduled("k").await;
        });
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;

        match g2.finish("k", Some(RunStop::FinalResponse)).await {
            Next::Scheduled => {}
            other => panic!("scheduled ticket beats pending, got {other:?}"),
        }
        waiter.await.unwrap();
        assert_eq!(g2.pending_len("k").await, 1);
        match g2.finish("k", Some(RunStop::FinalResponse)).await {
            Next::User { inputs, .. } => assert_eq!(texts(&inputs), ["left"]),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn clear_keeps_waiting_scheduled_tickets() {
        let g = std::sync::Arc::new(TurnGate::new());
        g.submit("k", input("start")).await;
        g.submit("k", input("1")).await;
        g.submit("k", input("2")).await;

        let g2 = g.clone();
        let waiter = tokio::spawn(async move {
            g2.begin_scheduled("k").await;
        });
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;

        assert_eq!(g.clear("k").await, 2, "clears user queue only");
        assert_eq!(g.pending_len("k").await, 0);

        match g.finish("k", Some(RunStop::Cancelled)).await {
            Next::Scheduled => {}
            other => panic!("waiting scheduled must survive /stop clear, got {other:?}"),
        }
        waiter.await.unwrap();
        assert!(g.is_active("k").await);
    }
}
