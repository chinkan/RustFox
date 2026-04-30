//! Generic autonomous task supervisor.
//! See `docs/plans/2026-04-30-autopilot-supervisor-design.md`.

pub mod artifact;
pub mod backend;
pub mod classifier;
pub mod intake;
pub mod job;
pub mod orchestrator;
pub mod planner;
pub mod policy;
pub mod redact;
pub mod reporter;
pub mod state;
pub mod store;
pub mod task;
pub mod verification;
pub mod workflow;
pub mod workspace;

use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;

use crate::supervisor::artifact::ArtifactManager;
use crate::supervisor::backend::{reasoning::ReasoningBackend, Registry};
use crate::supervisor::classifier::{Classifier, HeuristicClassifier};
use crate::supervisor::intake::IntakeRouter;
use crate::supervisor::orchestrator::Orchestrator;
use crate::supervisor::planner::Planner;
use crate::supervisor::policy::{PolicyDecision, PolicyEngine};
use crate::supervisor::reporter::Reporter;
use crate::supervisor::store::TaskStore;
use crate::supervisor::task::TaskStatus;
use crate::supervisor::verification::{VerificationEngine, VerificationOutcome};

pub enum SubmitOutcome {
    AutoExecutePlanned { task_id: String },
    NeedsClarification { task_id: String, question: String },
    NeedsApproval { task_id: String, reason: String },
}

impl SubmitOutcome {
    pub fn task_id(&self) -> String {
        match self {
            Self::AutoExecutePlanned { task_id }
            | Self::NeedsClarification { task_id, .. }
            | Self::NeedsApproval { task_id, .. } => task_id.clone(),
        }
    }
}

pub struct Supervisor {
    store: TaskStore,
    artifacts: Arc<ArtifactManager>,
    classifier: Box<dyn Classifier + Send + Sync>,
    policy: PolicyEngine,
    pub registry: Registry,
    pub workspace_mgr: Option<Arc<crate::supervisor::workspace::WorkspaceManager>>,
}

impl Supervisor {
    pub fn new_for_test(
        artifacts_root: PathBuf,
        conn: Arc<tokio::sync::Mutex<rusqlite::Connection>>,
    ) -> Self {
        Self {
            store: TaskStore::new(conn.clone()),
            artifacts: Arc::new(ArtifactManager::new(artifacts_root, conn)),
            classifier: Box::new(HeuristicClassifier),
            policy: PolicyEngine::default(),
            registry: Registry::new(),
            workspace_mgr: None,
        }
    }

    pub fn new_for_test_with_repo(
        artifacts_root: PathBuf,
        repo_path: PathBuf,
        conn: Arc<tokio::sync::Mutex<rusqlite::Connection>>,
    ) -> Self {
        let mut sup = Self::new_for_test(artifacts_root, conn);
        sup.workspace_mgr = Some(Arc::new(
            crate::supervisor::workspace::WorkspaceManager::new(repo_path, false),
        ));
        sup
    }

    /// Production constructor. Registry should be pre-populated with backends.
    pub fn new(
        artifacts_root: PathBuf,
        conn: Arc<tokio::sync::Mutex<rusqlite::Connection>>,
        registry: Registry,
        thresholds: crate::config::RiskThresholdsConfig,
    ) -> Self {
        Self {
            store: TaskStore::new(conn.clone()),
            artifacts: Arc::new(ArtifactManager::new(artifacts_root, conn)),
            classifier: Box::new(HeuristicClassifier),
            policy: PolicyEngine::with_thresholds(thresholds),
            registry,
            workspace_mgr: None,
        }
    }

    pub fn register_test_reasoning_backend<F, Fut>(&mut self, f: F)
    where
        F: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = anyhow::Result<String>> + Send + 'static,
    {
        self.registry
            .register(Arc::new(ReasoningBackend::new_with_executor(f)));
    }

    pub async fn execute_now(&self, task_id: &str) -> anyhow::Result<String> {
        let task = self
            .store
            .get(task_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("task not found"))?;

        // PLAN
        self.store
            .record_transition(
                task_id,
                TaskStatus::Route,
                TaskStatus::Plan,
                "supervisor",
                None,
            )
            .await?;
        let plan = Planner::new().plan(&task);
        self.artifacts
            .write_text(
                task_id,
                None,
                "plan",
                "plan.json",
                &serde_json::to_string_pretty(&serde_json::json!({
                    "jobs": plan.jobs.iter().map(|j| serde_json::json!({
                        "type": j.job_type, "backend": j.backend, "goal": j.goal,
                    })).collect::<Vec<_>>()
                }))?,
            )
            .await?;

        // PREPARE_WORKSPACE (only for code-modifying tasks when configured)
        let needs_ws = matches!(
            task.task_type,
            crate::supervisor::task::TaskType::CodeChange
                | crate::supervisor::task::TaskType::BugFix
                | crate::supervisor::task::TaskType::Refactor
        );
        let workspace_active = needs_ws && self.workspace_mgr.is_some();
        if workspace_active {
            if let Some(wm) = &self.workspace_mgr {
                self.store
                    .record_transition(
                        task_id,
                        TaskStatus::Plan,
                        TaskStatus::PrepareWorkspace,
                        "supervisor",
                        None,
                    )
                    .await?;
                let ws = wm.prepare(task_id, &task.title).await?;
                self.artifacts
                    .write_text(
                        task_id,
                        None,
                        "workspace",
                        "workspace.json",
                        &serde_json::to_string_pretty(&serde_json::json!({
                            "branch": ws.branch,
                            "path": ws.path.display().to_string(),
                        }))?,
                    )
                    .await?;
            }
        }

        // EXECUTE
        let pre_execute_state = if workspace_active {
            TaskStatus::PrepareWorkspace
        } else {
            TaskStatus::Plan
        };
        self.store
            .record_transition(
                task_id,
                pre_execute_state,
                TaskStatus::Execute,
                "supervisor",
                None,
            )
            .await?;
        let orch = Orchestrator::new(self.registry.clone(), self.store.clone());
        let res = orch.execute_plan(&task, plan).await?;
        let jobs = self.store.jobs_for_task(task_id).await?;

        // VERIFY
        // M3: regardless of orchestrator outcome we transition Execute->Verify
        // and let VerificationEngine produce the final pass/fail.
        let _ = res;
        self.store
            .record_transition(
                task_id,
                TaskStatus::Execute,
                TaskStatus::Verify,
                "supervisor",
                None,
            )
            .await?;
        let v = VerificationEngine.verify(&jobs);

        // REPORT + ARCHIVE
        let report = Reporter::render(&jobs);
        self.artifacts
            .write_text(task_id, None, "result", "report.md", &report)
            .await?;
        match v {
            VerificationOutcome::Passed => {
                self.store
                    .record_transition(
                        task_id,
                        TaskStatus::Verify,
                        TaskStatus::Report,
                        "supervisor",
                        None,
                    )
                    .await?;
                self.store
                    .record_transition(
                        task_id,
                        TaskStatus::Report,
                        TaskStatus::Archive,
                        "supervisor",
                        None,
                    )
                    .await?;
                self.store
                    .record_transition(
                        task_id,
                        TaskStatus::Archive,
                        TaskStatus::Done,
                        "supervisor",
                        None,
                    )
                    .await?;
                Ok(report)
            }
            VerificationOutcome::Failed(reason) => {
                self.store
                    .record_transition(
                        task_id,
                        TaskStatus::Verify,
                        TaskStatus::Failed,
                        "verifier",
                        Some(&reason),
                    )
                    .await?;
                Ok(format!("VERIFICATION FAILED: {reason}\n\n{report}"))
            }
        }
    }

    /// Mark a task as `Paused`. Records the transition unconditionally —
    /// the strict transition-table check is deferred to a later milestone.
    pub async fn pause(&self, task_id: &str) -> anyhow::Result<()> {
        let task = self
            .store
            .get(task_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("task not found"))?;
        self.store
            .record_transition(
                task_id,
                task.status,
                TaskStatus::Paused,
                "user",
                Some("paused"),
            )
            .await?;
        Ok(())
    }

    /// Resume a previously-paused task by re-entering `Execute` and running
    /// the rest of the pipeline.
    pub async fn resume(&self, task_id: &str) -> anyhow::Result<String> {
        let task = self
            .store
            .get(task_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("task not found"))?;
        if task.status == TaskStatus::Paused {
            self.store
                .record_transition(
                    task_id,
                    TaskStatus::Paused,
                    TaskStatus::Execute,
                    "user",
                    Some("resumed"),
                )
                .await?;
        }
        self.execute_now(task_id).await
    }

    /// IDs of tasks that look resumable on startup (paused or mid-pipeline).
    pub async fn resumable_task_ids(&self) -> anyhow::Result<Vec<String>> {
        self.store.list_resumable_task_ids().await
    }

    pub async fn state(&self, task_id: &str) -> anyhow::Result<TaskStatus> {
        Ok(self
            .store
            .get(task_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("task missing"))?
            .status)
    }

    pub fn artifacts(&self) -> &ArtifactManager {
        &self.artifacts
    }

    pub async fn submit(
        &self,
        platform: &str,
        user_id: &str,
        chat_id: Option<&str>,
        text: &str,
    ) -> Result<SubmitOutcome> {
        let mut task = IntakeRouter::normalize(text);
        self.store.create(&task, platform, user_id, chat_id).await?;
        self.artifacts
            .write_text(
                &task.id,
                None,
                "intake",
                "intake.json",
                &serde_json::to_string_pretty(&task)?,
            )
            .await?;

        // CLASSIFY
        self.store
            .record_transition(
                &task.id,
                TaskStatus::Intake,
                TaskStatus::Classify,
                "supervisor",
                Some("auto"),
            )
            .await?;
        let outcome = (*self.classifier).classify(text);
        task.task_type = outcome.task_type.clone();
        task.risk_level = outcome.risk_level.clone();
        task.execution_mode = outcome.execution_mode.clone();
        task.required_capabilities = outcome.required_capabilities.clone();
        self.store.update_classification(&task).await?;
        self.artifacts
            .write_text(
                &task.id,
                None,
                "classification",
                "classification.json",
                &serde_json::to_string_pretty(&serde_json::json!({
                    "task_type": task.task_type,
                    "risk_level": task.risk_level,
                    "execution_mode": task.execution_mode,
                    "required_capabilities": task.required_capabilities,
                    "confidence": outcome.confidence,
                }))?,
            )
            .await?;

        // ROUTE → POLICY
        self.store
            .record_transition(
                &task.id,
                TaskStatus::Classify,
                TaskStatus::Route,
                "supervisor",
                None,
            )
            .await?;
        let decision = self.policy.decide(&task);
        self.artifacts
            .write_text(
                &task.id,
                None,
                "policy",
                "policy.json",
                &serde_json::to_string_pretty(&serde_json::json!({
                    "decision": format!("{decision:?}")
                }))?,
            )
            .await?;

        Ok(match decision {
            PolicyDecision::AutoExecute => SubmitOutcome::AutoExecutePlanned { task_id: task.id },
            PolicyDecision::Clarify => {
                self.store
                    .record_transition(
                        &task.id,
                        TaskStatus::Route,
                        TaskStatus::Clarify,
                        "policy",
                        Some("ambiguous"),
                    )
                    .await?;
                SubmitOutcome::NeedsClarification {
                    task_id: task.id,
                    question: "I'm not sure what you want me to do — can you clarify?".into(),
                }
            }
            PolicyDecision::RequireApproval => SubmitOutcome::NeedsApproval {
                task_id: task.id,
                reason: "high-risk task".into(),
            },
            other => SubmitOutcome::NeedsApproval {
                task_id: task.id,
                reason: format!("{other:?}"),
            },
        })
    }
}
