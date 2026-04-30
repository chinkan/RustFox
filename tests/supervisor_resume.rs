use rustfox::supervisor::Supervisor;

#[tokio::test]
async fn supervisor_restores_paused_tasks_on_startup() {
    let dir = tempfile::tempdir().unwrap();
    let memory = rustfox::memory::MemoryStore::open_in_memory().unwrap();

    let task_id = {
        let mut sup = Supervisor::new_for_test(dir.path().into(), memory.connection());
        sup.register_test_reasoning_backend(|p| async move { Ok(p) });
        let outcome = sup
            .submit("telegram", "u", Some("c"), "summarize")
            .await
            .unwrap();
        let id = outcome.task_id();
        sup.pause(&id).await.unwrap();
        id
    };

    let sup2 = Supervisor::new_for_test(dir.path().into(), memory.connection());
    let resumable = sup2.resumable_task_ids().await.unwrap();
    assert_eq!(resumable.len(), 1);
    assert_eq!(resumable[0], task_id);
}

#[tokio::test]
async fn supervisor_resume_runs_task_to_completion() {
    let dir = tempfile::tempdir().unwrap();
    let memory = rustfox::memory::MemoryStore::open_in_memory().unwrap();

    let mut sup =
        rustfox::supervisor::Supervisor::new_for_test(dir.path().into(), memory.connection());
    sup.register_test_reasoning_backend(|p| async move { Ok(format!("resumed:{p}")) });

    let outcome = sup
        .submit("telegram", "u", Some("c"), "summarize the readme")
        .await
        .unwrap();
    let id = outcome.task_id();

    sup.pause(&id).await.unwrap();
    let report = sup.resume(&id).await.unwrap();
    assert!(report.contains("resumed:"));

    let final_state = sup.state(&id).await.unwrap();
    assert_eq!(final_state, rustfox::supervisor::task::TaskStatus::Done);
}
