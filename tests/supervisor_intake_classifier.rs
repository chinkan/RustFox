use rustfox::supervisor::{SubmitOutcome, Supervisor};

#[tokio::test]
async fn submit_persists_task_and_writes_artifacts() {
    let dir = tempfile::tempdir().unwrap();
    let memory = rustfox::memory::MemoryStore::open_in_memory().unwrap();
    let sup = Supervisor::new_for_test(dir.path().into(), memory.connection());

    let outcome = sup
        .submit(
            "telegram",
            "u1",
            Some("c1"),
            "summarize the file ./README.md",
        )
        .await
        .unwrap();

    assert!(matches!(outcome, SubmitOutcome::AutoExecutePlanned { .. }));
    let task_id = outcome.task_id();

    let arts = sup.artifacts().list(&task_id).await.unwrap();
    let kinds: Vec<_> = arts.iter().map(|a| a.kind.as_str()).collect();
    assert!(kinds.contains(&"intake"));
    assert!(kinds.contains(&"classification"));
    assert!(kinds.contains(&"policy"));
}
