use anyhow::{anyhow, Result};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::supervisor::backend::{Backend, BackendCapabilities};
use crate::supervisor::job::{Evidence, Job, JobOutput, JobStatus, JobType};

type ExecFn = Arc<
    dyn Fn(String) -> Pin<Box<dyn Future<Output = Result<String>> + Send>> + Send + Sync,
>;

pub struct ReasoningBackend {
    exec: ExecFn,
}

impl ReasoningBackend {
    /// Production constructor wrapping the real `Agent`.
    pub fn from_agent(
        agent: Arc<crate::agent::Agent>,
        default_user: String,
        default_chat: String,
    ) -> Self {
        let exec: ExecFn = Arc::new(move |prompt| {
            let agent = agent.clone();
            let user = default_user.clone();
            let chat = default_chat.clone();
            Box::pin(async move {
                let incoming = crate::platform::IncomingMessage {
                    platform: "supervisor".into(),
                    user_id: user,
                    chat_id: chat,
                    user_name: "supervisor".into(),
                    text: prompt,
                };
                agent
                    .process_message(&incoming, None, None)
                    .await
                    .map_err(|e| anyhow!("agent failed: {e:#}"))
            })
        });
        Self { exec }
    }

    /// Constructor that injects a custom executor closure.
    ///
    /// Intended for tests and harness wiring; production code should use
    /// [`ReasoningBackend::from_agent`].
    #[doc(hidden)]
    pub fn new_with_executor<F, Fut>(f: F) -> Self
    where
        F: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = anyhow::Result<String>> + Send + 'static,
    {
        let f = Arc::new(f);
        Self {
            exec: Arc::new(move |p| {
                let f = f.clone();
                Box::pin(async move { (f)(p).await })
            }),
        }
    }
}

#[async_trait::async_trait]
impl Backend for ReasoningBackend {
    fn name(&self) -> &str {
        "reasoning"
    }
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            reasoning: true,
            ..Default::default()
        }
    }
    fn can_handle(&self, jt: &JobType) -> bool {
        matches!(
            jt,
            JobType::PlannerJob | JobType::ExecutorJob | JobType::ReviewerJob | JobType::DocumentJob
        )
    }
    async fn run(&self, job: &mut Job) -> Result<JobOutput> {
        job.status = JobStatus::Running;
        let prompt = job.prompt.clone().unwrap_or_else(|| job.goal.clone());
        let summary = (self.exec)(prompt).await?;
        let evidence = vec![Evidence::OutputValidated {
            description: "non-empty reasoning output".into(),
        }];
        let status = if summary.is_empty() {
            JobStatus::Failed
        } else {
            JobStatus::Succeeded
        };
        job.status = status.clone();
        Ok(JobOutput {
            status,
            summary,
            evidence,
            errors: vec![],
            changed_files: vec![],
            next_step: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reasoning_backend_advertises_capabilities() {
        let b = ReasoningBackend::new_with_executor(|prompt| async move {
            Ok(format!("echo:{prompt}"))
        });
        let caps = b.capabilities();
        assert!(caps.reasoning);
        assert!(!caps.shell);

        let mut job = crate::supervisor::job::Job::new(
            "task1",
            crate::supervisor::job::JobType::PlannerJob,
            "reasoning",
            "plan it",
        );
        job.prompt = Some("hello".into());
        let out = b.run(&mut job).await.unwrap();
        assert!(out.summary.starts_with("echo:hello"));
    }
}
