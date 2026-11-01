//! ADR-0020 / ADR-0052: one gate per conversation key (`session_key(bot_id, user_id)`).
//!
//! Two lanes share the key. The **interactive** lane is live chat. Messages that
//! arrive while it is busy wait in a FIFO (cap [`QUEUE_CAP`]) and steer that
//! turn at its next step, or start the next interactive turn when it ends.
//! The **scheduled** lane is one scheduler job. Inbound chat while it runs
//! starts or continues the interactive lane immediately — it is never a steer
//! into the schedule loop and it is not held until the job finishes (#338).
//! A scheduled job still waits to *start* until the interactive lane is idle
//! (ADR-0020 Q8). Nothing is dropped silently: every message is started,
//! accepted, or refused. In-memory only (v1).

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
    /// Interactive lane was idle (a scheduled lane may still be running).
    /// A user turn is now active. Run it with these inputs (any leftover
    /// queue first, then the new message — arrival order).
    Start(Vec<QueuedInput>),
    /// Queued behind an active **interactive** turn (👀 mid-run steer / queue UX).
    /// A scheduled lane running beside it does not change this (#338).
    Accepted,
    /// Queue full: not stored (Q4 refusal). The input is handed back.
    Full(QueuedInput),
}

/// What runs after one lane ends ([`TurnGate::finish_user`] /
/// [`TurnGate::finish_scheduled`]).
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
    /// Interactive (user chat) turn is running.
    user: bool,
    /// Scheduled turn is running. Independent of `user` (#338).
    scheduled: bool,
    pending: VecDeque<QueuedInput>,
    waiting_scheduled: VecDeque<tokio::sync::oneshot::Sender<()>>,
    auto_continue_used: bool,
}

/// Hand the scheduled lane to the oldest waiter. Caller must only invoke this
/// when `scheduled` is currently false. On success the lane is marked held.
fn release_scheduled_ticket(st: &mut KeyState) -> Option<Next> {
    while let Some(ticket) = st.waiting_scheduled.pop_front() {
        if ticket.send(()).is_ok() {
            st.scheduled = true;
            return Some(Next::Scheduled);
        }
    }
    None
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
    ///
    /// Interactive lane idle → [`Submit::Start`], even when a scheduled lane
    /// is running (#338). Interactive lane busy → [`Submit::Accepted`]
    /// (steer/queue). The scheduled lane is never the steer target.
    pub async fn submit(&self, key: &str, input: QueuedInput) -> Submit {
        let mut keys = self.keys.lock().await;
        let st = keys.entry(key.to_string()).or_default();
        if !st.user {
            st.user = true;
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

    /// ADR-0020 Decision 8: claim the scheduled lane.
    ///
    /// Starts immediately when both lanes are idle. Waits while the
    /// interactive lane is busy, or while another scheduled run already holds
    /// the lane. An in-flight scheduled run does **not** block [`submit`]
    /// (ADR-0052 / #338).
    pub async fn begin_scheduled(&self, key: &str) {
        loop {
            let rx = {
                let mut keys = self.keys.lock().await;
                let st = keys.entry(key.to_string()).or_default();
                if !st.user && !st.scheduled {
                    st.scheduled = true;
                    return;
                }
                let (tx, rx) = tokio::sync::oneshot::channel();
                st.waiting_scheduled.push_back(tx);
                rx
            };
            // The releaser set `scheduled` before sending.
            if rx.await.is_ok() {
                return;
            }
            // Sender dropped without handoff — retry.
        }
    }

    /// Steer drain for the active **interactive** lane.
    ///
    /// A scheduled run must not call this. Use [`Self::drain_for`] with
    /// [`TurnKind::Scheduled`] to observe that the schedule lane never
    /// receives queued chat (#338).
    pub async fn drain(&self, key: &str) -> Vec<QueuedInput> {
        self.drain_for(key, TurnKind::User).await
    }

    /// Drain queued inputs for `kind`. [`TurnKind::Scheduled`] is always empty:
    /// inbound chat is not a steer into the schedule loop.
    pub async fn drain_for(&self, key: &str, kind: TurnKind) -> Vec<QueuedInput> {
        if kind != TurnKind::User {
            return Vec::new();
        }
        let mut keys = self.keys.lock().await;
        match keys.get_mut(key) {
            Some(st) if st.user => st.pending.drain(..).collect(),
            _ => Vec::new(),
        }
    }

    /// The interactive lane ended with `stop` (`None` = the turn errored).
    ///
    /// Does not clear a scheduled lane that is still running. A waiting
    /// schedule is released only when the scheduled lane is free, and it
    /// still starts before leftover pending (ADR-0020 Decision 5.1).
    pub async fn finish_user(&self, key: &str, stop: Option<RunStop>) -> Next {
        let mut keys = self.keys.lock().await;
        let st = keys.entry(key.to_string()).or_default();
        let was_user = st.user;
        st.user = false;
        let user_max_iter = was_user && stop == Some(RunStop::MaxIterations);
        if matches!(stop, Some(RunStop::FinalResponse | RunStop::Cancelled)) {
            st.auto_continue_used = false;
        }

        if !st.scheduled {
            if let Some(next) = release_scheduled_ticket(st) {
                return next;
            }
        }

        if st.pending.is_empty() {
            return Next::Idle;
        }
        if user_max_iter && st.auto_continue_used {
            return Next::StepLimit {
                n: st.pending.len(),
            };
        }
        st.auto_continue_used = user_max_iter;
        st.user = true;
        Next::User {
            inputs: st.pending.drain(..).collect(),
            auto_continue: user_max_iter,
        }
    }

    /// The scheduled lane ended. Does not clear an interactive lane that is
    /// still running, and does not take its pending queue (#338).
    ///
    /// When the interactive lane is idle, leftover pending (messages that
    /// waited behind a schedule that was itself waiting on chat — Decision
    /// 5.1) start one user turn. A further waiting schedule still goes first.
    pub async fn finish_scheduled(&self, key: &str) -> Next {
        let mut keys = self.keys.lock().await;
        let st = keys.entry(key.to_string()).or_default();
        st.scheduled = false;
        if st.user {
            return Next::Idle;
        }
        if let Some(next) = release_scheduled_ticket(st) {
            return next;
        }
        if st.pending.is_empty() {
            return Next::Idle;
        }
        st.auto_continue_used = false;
        st.user = true;
        Next::User {
            inputs: st.pending.drain(..).collect(),
            auto_continue: false,
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
            .is_some_and(|s| s.user || s.scheduled)
    }

    pub async fn interactive_active(&self, key: &str) -> bool {
        self.keys.lock().await.get(key).is_some_and(|s| s.user)
    }

    pub async fn scheduled_active(&self, key: &str) -> bool {
        self.keys.lock().await.get(key).is_some_and(|s| s.scheduled)
    }

    /// Interactive lane if it is running, otherwise the scheduled lane.
    /// `None` when both are idle. When both run, this is [`TurnKind::User`]
    /// because steer applies only to that lane.
    pub async fn active_kind(&self, key: &str) -> Option<TurnKind> {
        self.keys.lock().await.get(key).and_then(|s| {
            if s.user {
                Some(TurnKind::User)
            } else if s.scheduled {
                Some(TurnKind::Scheduled)
            } else {
                None
            }
        })
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
        match g.finish_user("k", Some(RunStop::FinalResponse)).await {
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
            g.finish_user("k", Some(RunStop::FinalResponse)).await,
            Next::Idle
        ));
        assert!(!g.is_active("k").await);
    }

    #[tokio::test]
    async fn errored_turn_still_runs_leftovers() {
        let g = TurnGate::new();
        g.submit("k", input("start")).await;
        g.submit("k", input("1")).await;
        assert!(matches!(g.finish_user("k", None).await, Next::User { .. }));
    }

    #[tokio::test]
    async fn max_iterations_auto_continues_once_then_step_limit() {
        let g = TurnGate::new();
        g.submit("k", input("start")).await;
        g.submit("k", input("1")).await;
        g.submit("k", input("2")).await;
        match g.finish_user("k", Some(RunStop::MaxIterations)).await {
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
        match g.finish_user("k", Some(RunStop::MaxIterations)).await {
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
            g.finish_user("k", Some(RunStop::MaxIterations)).await,
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
        g.finish_user("k", Some(RunStop::MaxIterations)).await; // auto turn
        g.submit("k", input("2")).await;
        // auto turn answers normally → its leftovers start a plain turn
        assert!(matches!(
            g.finish_user("k", Some(RunStop::FinalResponse)).await,
            Next::User {
                auto_continue: false,
                ..
            }
        ));
        g.submit("k", input("3")).await;
        assert!(matches!(
            g.finish_user("k", Some(RunStop::MaxIterations)).await,
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
            let fin = g.finish_user("k", Some(RunStop::FinalResponse)).await;
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
        assert!(g.scheduled_active("k").await);
        assert!(matches!(g.finish_scheduled("k").await, Next::Idle));
        assert!(!g.is_active("k").await);
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

        match g.finish_user("k", Some(RunStop::FinalResponse)).await {
            Next::Scheduled => {}
            other => panic!("expected Scheduled handoff, got {other:?}"),
        }
        waiter.await.unwrap();
        assert!(started.load(Ordering::SeqCst));
        assert!(g.is_active("k").await, "scheduled owns the key");
        // Leftover pending waits behind the scheduled turn (Decision 5.1).
        assert_eq!(g.pending_len("k").await, 1);
        // Scheduled lane has no steer drain; leftovers are not injected into it.
        assert!(g.drain("k").await.is_empty());
        assert!(g.drain_for("k", TurnKind::Scheduled).await.is_empty());
        assert!(!g.interactive_active("k").await);

        match g.finish_scheduled("k").await {
            Next::User { inputs, .. } => assert_eq!(texts(&inputs), ["mid"]),
            other => panic!("{other:?}"),
        }
    }

    /// #338: schedule-busy inbound starts the interactive lane. It is not a
    /// steer into the schedule, and it is not held until the job finishes.
    /// A later message steers only that interactive lane.
    #[tokio::test]
    async fn schedule_busy_inbound_starts_interactive_not_schedule_steer() {
        let g = TurnGate::new();
        g.begin_scheduled("k").await;
        assert!(g.scheduled_active("k").await);
        assert!(!g.interactive_active("k").await);

        match g.submit("k", input("hello")).await {
            Submit::Start(v) => assert_eq!(texts(&v), ["hello"]),
            other => panic!("expected interactive Start, got {other:?}"),
        }
        assert!(g.interactive_active("k").await);
        assert!(g.scheduled_active("k").await, "schedule keeps its lane");
        assert_eq!(g.pending_len("k").await, 0);
        assert!(
            g.drain_for("k", TurnKind::Scheduled).await.is_empty(),
            "schedule lane has no steer drain"
        );

        assert!(matches!(
            g.submit("k", input("more")).await,
            Submit::Accepted
        ));
        assert!(g.drain_for("k", TurnKind::Scheduled).await.is_empty());
        assert_eq!(texts(&g.drain("k").await), ["more"]);

        // Schedule ending must not take the interactive lane down with it.
        assert!(matches!(g.finish_scheduled("k").await, Next::Idle));
        assert!(g.interactive_active("k").await);
        assert!(!g.scheduled_active("k").await);
        assert!(matches!(
            g.finish_user("k", Some(RunStop::FinalResponse)).await,
            Next::Idle
        ));
        assert!(!g.is_active("k").await);
    }

    /// Steer (👀 / Accepted) only when the interactive lane itself is busy.
    #[tokio::test]
    async fn steer_only_when_interactive_lane_is_busy() {
        let g = TurnGate::new();
        g.begin_scheduled("sched").await;
        assert!(matches!(
            g.submit("sched", input("a")).await,
            Submit::Start(_)
        ));
        assert!(matches!(
            g.submit("sched", input("b")).await,
            Submit::Accepted
        ));
        assert!(matches!(
            g.submit("sched", input("c")).await,
            Submit::Accepted
        ));
        assert!(g.drain_for("sched", TurnKind::Scheduled).await.is_empty());
        assert_eq!(texts(&g.drain("sched").await), ["b", "c"]);

        let idle = TurnGate::new();
        assert!(matches!(
            idle.submit("u", input("start")).await,
            Submit::Start(_)
        ));
        assert!(matches!(
            idle.submit("u", input("mid")).await,
            Submit::Accepted
        ));
        assert_eq!(texts(&idle.drain("u").await), ["mid"]);
    }

    /// Schedule finish must not start a second interactive turn for messages
    /// that already belong to the running interactive lane.
    #[tokio::test]
    async fn schedule_finish_does_not_steal_interactive_queue() {
        let g = TurnGate::new();
        g.begin_scheduled("k").await;
        assert!(matches!(
            g.submit("k", input("chat")).await,
            Submit::Start(_)
        ));
        assert!(matches!(
            g.submit("k", input("later")).await,
            Submit::Accepted
        ));
        match g.finish_scheduled("k").await {
            Next::Idle => {}
            other => panic!("schedule finish stole the interactive queue: {other:?}"),
        }
        assert!(g.interactive_active("k").await);
        assert!(!g.scheduled_active("k").await);
        assert_eq!(texts(&g.drain("k").await), ["later"]);
    }

    /// Interactive finish while a schedule is running leaves the schedule lane up.
    #[tokio::test]
    async fn interactive_finish_leaves_scheduled_lane_running() {
        let g = TurnGate::new();
        g.begin_scheduled("k").await;
        assert!(matches!(
            g.submit("k", input("chat")).await,
            Submit::Start(_)
        ));
        assert!(matches!(
            g.finish_user("k", Some(RunStop::FinalResponse)).await,
            Next::Idle
        ));
        assert!(!g.interactive_active("k").await);
        assert!(g.scheduled_active("k").await);
        assert!(matches!(g.finish_scheduled("k").await, Next::Idle));
        assert!(!g.is_active("k").await);
    }

    /// Inbound racing the schedule's finish is never dropped and never steered
    /// into the schedule: it starts the interactive lane.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn inbound_racing_schedule_finish_starts_interactive() {
        for round in 0..100 {
            let g = std::sync::Arc::new(TurnGate::new());
            g.begin_scheduled("k").await;
            let g2 = g.clone();
            let sub = tokio::spawn(async move {
                tokio::task::yield_now().await;
                g2.submit("k", input("late")).await
            });
            let fin = g.finish_scheduled("k").await;
            let sub = sub.await.unwrap();
            let ok = match (&fin, &sub) {
                (Next::Idle, Submit::Start(v)) => texts(v) == ["late"],
                _ => false,
            };
            assert!(
                ok,
                "round {round}: inbound lost or steered ({fin:?}, {sub:?})"
            );
            assert!(g.interactive_active("k").await, "round {round}");
            assert!(!g.scheduled_active("k").await, "round {round}");
        }
    }

    /// Cap still applies to steers behind the interactive lane, including when
    /// a schedule is running beside it. The message that *starts* the lane
    /// does not consume a slot.
    #[tokio::test]
    async fn interactive_queue_cap_holds_while_schedule_runs() {
        let g = TurnGate::new();
        g.begin_scheduled("k").await;
        assert!(matches!(
            g.submit("k", input("start")).await,
            Submit::Start(_)
        ));
        for i in 0..QUEUE_CAP {
            assert!(matches!(
                g.submit("k", input(&i.to_string())).await,
                Submit::Accepted
            ));
        }
        match g.submit("k", input("overflow")).await {
            Submit::Full(back) => assert_eq!(back.text, "overflow"),
            other => panic!("expected Full, got {other:?}"),
        }
        assert_eq!(g.pending_len("k").await, QUEUE_CAP);
        assert!(g.drain_for("k", TurnKind::Scheduled).await.is_empty());
        assert_eq!(g.drain("k").await.len(), QUEUE_CAP);
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

        match g2.finish_user("k", Some(RunStop::FinalResponse)).await {
            Next::Scheduled => {}
            other => panic!("scheduled ticket beats pending, got {other:?}"),
        }
        waiter.await.unwrap();
        assert_eq!(g2.pending_len("k").await, 1);
        assert!(g2.drain_for("k", TurnKind::Scheduled).await.is_empty());
        match g2.finish_scheduled("k").await {
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

        match g.finish_user("k", Some(RunStop::Cancelled)).await {
            Next::Scheduled => {}
            other => panic!("waiting scheduled must survive /stop clear, got {other:?}"),
        }
        waiter.await.unwrap();
        assert!(g.is_active("k").await);
    }
}
