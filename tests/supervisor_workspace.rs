use rustfox::supervisor::Supervisor;

#[tokio::test]
async fn rigorous_code_task_creates_workspace_before_execute() {
    let dir = tempfile::tempdir().unwrap();
    init_git_repo(dir.path()).await;

    let memory = rustfox::memory::MemoryStore::open_in_memory().unwrap();
    let mut sup = Supervisor::new_for_test_with_repo(
        dir.path().into(),
        dir.path().into(),
        memory.connection(),
    );
    sup.register_test_reasoning_backend(|p| async move { Ok(p) });

    let outcome = sup
        .submit(
            "telegram",
            "u1",
            Some("c1"),
            "refactor module foo to be testable",
        )
        .await
        .unwrap();
    let id = outcome.task_id();
    sup.execute_now(&id).await.unwrap();

    let arts = sup.artifacts().list(&id).await.unwrap();
    let kinds: Vec<_> = arts.iter().map(|a| a.kind.as_str()).collect();
    assert!(
        kinds.contains(&"workspace"),
        "missing workspace artifact, got: {kinds:?}"
    );
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
