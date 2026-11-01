//! Schedule lane vs interactive lane (ADR-0052 / #338).
//!
//! A scheduled run on a session key must not capture inbound chat. The first
//! message starts the interactive lane; a later one steers only that lane.
//! The schedule's drain stays empty, and finishing either lane leaves the
//! other running.

use rustfox::agent::RunStop;
use rustfox::turn_gate::{Next, QueuedInput, Submit, TurnGate, TurnKind};

fn input(text: &str) -> QueuedInput {
    QueuedInput {
        text: text.to_string(),
        ..Default::default()
    }
}

fn texts(v: &[QueuedInput]) -> Vec<&str> {
    v.iter().map(|i| i.text.as_str()).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn schedule_busy_inbound_is_interactive_not_schedule_steer() {
    let gate = std::sync::Arc::new(TurnGate::new());
    let key = "main:42";
    gate.begin_scheduled(key).await;

    let started = std::sync::Arc::new(tokio::sync::Notify::new());
    let started2 = started.clone();
    let gate_chat = gate.clone();
    let chat = tokio::spawn(async move {
        started2.notified().await;
        gate_chat.submit(key, input("while the job runs")).await
    });

    assert!(gate.scheduled_active(key).await);
    assert!(!gate.interactive_active(key).await);
    started.notify_one();

    match chat.await.unwrap() {
        Submit::Start(v) => assert_eq!(texts(&v), ["while the job runs"]),
        other => panic!("expected interactive Start, got {other:?}"),
    }
    assert!(gate.interactive_active(key).await);
    assert!(gate.scheduled_active(key).await);

    assert!(matches!(
        gate.submit(key, input("and another")).await,
        Submit::Accepted
    ));
    assert!(
        gate.drain_for(key, TurnKind::Scheduled).await.is_empty(),
        "inbound must not steer the schedule loop"
    );
    assert_eq!(texts(&gate.drain(key).await), ["and another"]);

    assert!(matches!(gate.finish_scheduled(key).await, Next::Idle));
    assert!(gate.interactive_active(key).await);
    assert!(!gate.scheduled_active(key).await);

    assert!(matches!(
        gate.finish_user(key, Some(RunStop::FinalResponse)).await,
        Next::Idle
    ));
    assert!(!gate.is_active(key).await);
}

#[tokio::test]
async fn schedule_still_waits_to_start_until_interactive_is_idle() {
    let gate = std::sync::Arc::new(TurnGate::new());
    let key = "main:7";
    assert!(matches!(
        gate.submit(key, input("chatting")).await,
        Submit::Start(_)
    ));

    let gate2 = gate.clone();
    let waiter = tokio::spawn(async move {
        gate2.begin_scheduled(key).await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    assert!(
        !waiter.is_finished(),
        "scheduled start waits behind an interactive turn"
    );
    assert!(matches!(
        gate.submit(key, input("steer-me")).await,
        Submit::Accepted
    ));

    assert!(matches!(
        gate.finish_user(key, Some(RunStop::FinalResponse)).await,
        Next::Scheduled
    ));
    waiter.await.unwrap();
    assert!(gate.scheduled_active(key).await);
    assert!(!gate.interactive_active(key).await);
    assert!(gate.drain_for(key, TurnKind::Scheduled).await.is_empty());
    assert_eq!(gate.pending_len(key).await, 1);

    match gate.finish_scheduled(key).await {
        Next::User { inputs, .. } => assert_eq!(texts(&inputs), ["steer-me"]),
        other => panic!("{other:?}"),
    }
}
