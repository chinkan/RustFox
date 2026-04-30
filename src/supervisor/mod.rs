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
pub mod state;
pub mod store;
pub mod task;
pub mod workflow;

use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;

use crate::supervisor::artifact::ArtifactManager;
use crate::supervisor::classifier::{Classifier, HeuristicClassifier};
use crate::supervisor::intake::IntakeRouter;
use crate::supervisor::policy::{PolicyDecision, PolicyEngine};
use crate::supervisor::store::TaskStore;
use crate::supervisor::task::TaskStatus;

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
            policy: PolicyEngine,
        }
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
