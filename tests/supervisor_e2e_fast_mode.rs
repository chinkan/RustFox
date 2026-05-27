use rustfox::supervisor::{SubmitOutcome, Supervisor};

#[tokio::test]
async fn fast_mode_runs_to_completion_and_reports() {
    let dir = tempfile::tempdir().unwrap();
    let memory = rustfox::memory::MemoryStore::open_in_memory().unwrap();
    let mut sup = Supervisor::new_for_test(dir.path().into(), memory.connection());
    sup.register_test_reasoning_backend(|p| async move { Ok(format!("done:{p}")) });

    let outcome = sup
        .submit("telegram", "u1", Some("c1"), "summarize the readme")
        .await
        .unwrap();
    let task_id = outcome.task_id();
    assert!(matches!(outcome, SubmitOutcome::AutoExecutePlanned { .. }));

    let report = sup.execute_now(&task_id).await.unwrap();
    assert!(report.contains("done:"));
    let final_state = sup.state(&task_id).await.unwrap();
    assert_eq!(final_state, rustfox::supervisor::task::TaskStatus::Done);
}
