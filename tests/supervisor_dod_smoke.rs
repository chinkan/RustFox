//! Definition-of-Done smoke tests for the autopilot supervisor.
//!
//! Each test exercises the full pipeline (intake → classify → policy →
//! plan → execute → verify → report → archive → done) for a different
//! workflow class so a regression in any stage trips at least one test.

use rustfox::supervisor::task::TaskStatus;
use rustfox::supervisor::{SubmitOutcome, Supervisor};

#[tokio::test]
async fn dod_general_assistant_fast_mode() {
    let dir = tempfile::tempdir().unwrap();
    let memory = rustfox::memory::MemoryStore::open_in_memory().unwrap();
    let mut sup = Supervisor::new_for_test(dir.path().into(), memory.connection());
    sup.register_test_reasoning_backend(|p| async move { Ok(format!("answered:{p}")) });

    let outcome = sup
        .submit("telegram", "u", Some("c"), "summarize the readme")
        .await
        .unwrap();
    let id = outcome.task_id();
    assert!(matches!(outcome, SubmitOutcome::AutoExecutePlanned { .. }));

    let report = sup.execute_now(&id).await.unwrap();
    assert!(report.contains("answered:"));
    assert_eq!(sup.state(&id).await.unwrap(), TaskStatus::Done);

    let kinds: Vec<String> = sup
        .artifacts()
        .list(&id)
        .await
        .unwrap()
        .iter()
        .map(|a| a.kind.clone())
        .collect();
    for needed in ["intake", "classification", "policy", "plan", "result"] {
        assert!(
            kinds.contains(&needed.to_string()),
            "missing artifact kind {needed} (got {kinds:?})"
        );
    }
}

#[tokio::test]
async fn dod_research_workflow_artifacts_present() {
    let dir = tempfile::tempdir().unwrap();
    let memory = rustfox::memory::MemoryStore::open_in_memory().unwrap();
    let mut sup = Supervisor::new_for_test(dir.path().into(), memory.connection());
    sup.register_test_reasoning_backend(|p| async move { Ok(format!("research:{p}")) });
    let id = sup
        .submit("telegram", "u", Some("c"), "research async runtimes")
        .await
        .unwrap()
        .task_id();
    sup.execute_now(&id).await.unwrap();

    let kinds: Vec<String> = sup
        .artifacts()
        .list(&id)
        .await
        .unwrap()
        .iter()
        .map(|a| a.kind.clone())
        .collect();
    for needed in ["intake", "classification", "policy", "plan", "result"] {
        assert!(
            kinds.contains(&needed.to_string()),
            "missing artifact kind: {needed}"
        );
    }
}

#[tokio::test]
async fn dod_writing_workflow_completes() {
    let dir = tempfile::tempdir().unwrap();
    let memory = rustfox::memory::MemoryStore::open_in_memory().unwrap();
    let mut sup = Supervisor::new_for_test(dir.path().into(), memory.connection());
    sup.register_test_reasoning_backend(|p| async move { Ok(format!("draft:{p}")) });
    let id = sup
        .submit("telegram", "u", Some("c"), "write a blog post about Rust")
        .await
        .unwrap()
        .task_id();
    sup.execute_now(&id).await.unwrap();
    assert_eq!(sup.state(&id).await.unwrap(), TaskStatus::Done);
}

#[tokio::test]
async fn dod_high_risk_task_requires_approval() {
    // We can't directly trigger High via the heuristic classifier, so we
    // exercise the equivalent gate: a Medium-risk request under strict
    // thresholds must surface as `NeedsApproval`.
    let dir = tempfile::tempdir().unwrap();
    let memory = rustfox::memory::MemoryStore::open_in_memory().unwrap();
    let strict = Supervisor::new(
        dir.path().into(),
        memory.connection(),
        rustfox::supervisor::backend::Registry::new(),
        rustfox::config::RiskThresholdsConfig {
            require_approval_for_medium: true,
            ..Default::default()
        },
    );
    let outcome = strict
        .submit("telegram", "u", Some("c"), "refactor module foo")
        .await
        .unwrap();
    assert!(matches!(outcome, SubmitOutcome::NeedsApproval { .. }));
}

#[tokio::test]
async fn dod_rigorous_mode_visits_review_state() {
    use rustfox::supervisor::task::TaskStatus;
    let dir = tempfile::tempdir().unwrap();
    let memory = rustfox::memory::MemoryStore::open_in_memory().unwrap();

    // Use repo-aware constructor so workspace stage works for code task
    let repo = tempfile::tempdir().unwrap();
    init_git_repo(repo.path()).await;

    let mut sup = rustfox::supervisor::Supervisor::new_for_test_with_repo(
        dir.path().into(),
        repo.path().into(),
        memory.connection(),
    );
    sup.register_test_reasoning_backend(|p| async move { Ok(format!("ok:{p}")) });

    // "refactor X" → Refactor + Rigorous
    let id = sup
        .submit("telegram", "u", Some("c"), "refactor module foo")
        .await
        .unwrap()
        .task_id();
    sup.execute_now(&id).await.unwrap();
    assert_eq!(sup.state(&id).await.unwrap(), TaskStatus::Done);

    // Verify the audit log contains the Review state
    let mem_conn = memory.connection();
    let conn = mem_conn.lock().await;
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sup_transitions WHERE task_id=?1 AND to_state='\"REVIEW\"'",
            [&id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "Rigorous mode must record Execute -> Review");
}

async fn init_git_repo(p: &std::path::Path) {
    let run = |args: &[&str]| {
        let mut cmd = std::process::Command::new("git");
        cmd.args(args).current_dir(p);
        cmd.env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com");
        cmd.env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com");
        let _ = cmd.output().expect("git");
    };
    run(&["init", "-q", "-b", "main"]);
    run(&["config", "user.email", "test@example.com"]);
    run(&["config", "user.name", "test"]);
    tokio::fs::write(p.join("README.md"), "init").await.unwrap();
    run(&["add", "."]);
    run(&["commit", "-q", "-m", "init"]);
}

#[tokio::test]
async fn dod_resumes_from_paused_state() {
    let dir = tempfile::tempdir().unwrap();
    let memory = rustfox::memory::MemoryStore::open_in_memory().unwrap();
    let mut sup = Supervisor::new_for_test(dir.path().into(), memory.connection());
    sup.register_test_reasoning_backend(|p| async move { Ok(format!("done:{p}")) });
    let id = sup
        .submit("telegram", "u", Some("c"), "summarize this")
        .await
        .unwrap()
        .task_id();
    sup.pause(&id).await.unwrap();
    let report = sup.resume(&id).await.unwrap();
    assert!(report.contains("done:"));
    assert_eq!(sup.state(&id).await.unwrap(), TaskStatus::Done);
}
