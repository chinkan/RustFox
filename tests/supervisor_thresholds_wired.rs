use rustfox::config::RiskThresholdsConfig;
use rustfox::supervisor::{SubmitOutcome, Supervisor};

#[tokio::test]
async fn production_supervisor_applies_risk_thresholds_from_config() {
    let dir = tempfile::tempdir().unwrap();
    let memory = rustfox::memory::MemoryStore::open_in_memory().unwrap();
    let strict_thresholds = RiskThresholdsConfig {
        require_approval_for_medium: true,
        ..Default::default()
    };
    let sup = Supervisor::new(
        dir.path().into(),
        memory.connection(),
        rustfox::supervisor::backend::Registry::new(),
        strict_thresholds,
    );

    // "refactor X" → TaskType::Refactor + RiskLevel::Medium per HeuristicClassifier
    let outcome = sup
        .submit("telegram", "u", Some("c"), "refactor module foo")
        .await
        .unwrap();
    assert!(
        matches!(outcome, SubmitOutcome::NeedsApproval { .. }),
        "medium-risk task should require approval under strict thresholds"
    );
}

#[tokio::test]
async fn production_supervisor_default_thresholds_auto_execute_medium() {
    let dir = tempfile::tempdir().unwrap();
    let memory = rustfox::memory::MemoryStore::open_in_memory().unwrap();
    let sup = Supervisor::new(
        dir.path().into(),
        memory.connection(),
        rustfox::supervisor::backend::Registry::new(),
        RiskThresholdsConfig::default(),
    );
    let outcome = sup
        .submit("telegram", "u", Some("c"), "refactor module foo")
        .await
        .unwrap();
    assert!(matches!(outcome, SubmitOutcome::AutoExecutePlanned { .. }));
}
