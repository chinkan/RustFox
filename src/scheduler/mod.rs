pub mod reminders;
pub mod reruns;
pub mod tasks;

use anyhow::{Context, Result};
use std::time::Duration;
use tokio_cron_scheduler::{Job, JobScheduler};
use tracing::info;
use uuid::Uuid;

/// Wrapper around tokio-cron-scheduler for background tasks
pub struct Scheduler {
    inner: JobScheduler,
}

impl Scheduler {
    pub async fn new() -> Result<Self> {
        let inner = JobScheduler::new()
            .await
            .context("Failed to create job scheduler")?;
        Ok(Self { inner })
    }

    /// Add a recurring cron job. Returns the job's UUID (for cancellation).
    pub async fn add_cron_job<F>(&self, cron_expr: &str, name: &str, task: F) -> Result<Uuid>
    where
        F: Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
            + Send
            + Sync
            + 'static,
    {
        let job_name = name.to_string();
        let job = Job::new_async(cron_expr, move |_uuid, _lock| {
            let name = job_name.clone();
            let fut = task();
            Box::pin(async move {
                info!("Running scheduled task: {}", name);
                fut.await;
            })
        })
        .with_context(|| format!("Failed to create cron job: {}", name))?;

        let id = self
            .inner
            .add(job)
            .await
            .with_context(|| format!("Failed to add job: {}", name))?;

        info!("Scheduled task '{}' with cron: {}", name, cron_expr);
        Ok(id)
    }

    /// Add a one-shot job that fires once after `delay`. Returns the job's UUID.
    #[allow(dead_code)]
    pub async fn add_one_shot_job<F>(&self, delay: Duration, name: &str, task: F) -> Result<Uuid>
    where
        F: FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
            + Send
            + Sync
            + 'static,
    {
        let job_name = name.to_string();
        let mut task_opt = Some(task);
        let job = Job::new_one_shot_async(delay, move |_uuid, _lock| {
            let name = job_name.clone();
            let fut = task_opt.take().map(|f| f());
            Box::pin(async move {
                info!("Running one-shot task: {}", name);
                if let Some(f) = fut {
                    f.await;
                }
            })
        })
        .with_context(|| format!("Failed to create one-shot job: {}", name))?;

        let id = self
            .inner
            .add(job)
            .await
            .with_context(|| format!("Failed to add one-shot job: {}", name))?;

        info!("One-shot task '{}' scheduled in {:?}", name, delay);
        Ok(id)
    }

    /// Remove a job by its UUID.
    #[allow(dead_code)]
    pub async fn remove_job(&self, id: Uuid) -> Result<()> {
        self.inner
            .remove(&id)
            .await
            .with_context(|| format!("Failed to remove job: {}", id))?;
        Ok(())
    }

    /// Start the scheduler
    pub async fn start(&self) -> Result<()> {
        self.inner
            .start()
            .await
            .context("Failed to start scheduler")?;
        info!("Scheduler started");
        Ok(())
    }

    /// Shutdown the scheduler
    #[allow(dead_code)]
    pub async fn shutdown(&mut self) -> Result<()> {
        self.inner
            .shutdown()
            .await
            .context("Failed to shutdown scheduler")?;
        info!("Scheduler stopped");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn test_add_cron_job_returns_uuid() {
        let scheduler = Scheduler::new().await.unwrap();
        scheduler.start().await.unwrap();
        let id = scheduler
            .add_cron_job("0 * * * * *", "test-cron", || Box::pin(async {}))
            .await
            .unwrap();
        // Uuid is non-zero
        assert_ne!(id.as_u128(), 0);
    }

    #[tokio::test]
    async fn test_add_one_shot_job_returns_uuid() {
        let scheduler = Scheduler::new().await.unwrap();
        scheduler.start().await.unwrap();
        let id = scheduler
            .add_one_shot_job(Duration::from_secs(3600), "test-oneshot", || {
                Box::pin(async {})
            })
            .await
            .unwrap();
        assert_ne!(id.as_u128(), 0);
    }

    #[tokio::test]
    async fn test_remove_job_does_not_error() {
        let scheduler = Scheduler::new().await.unwrap();
        scheduler.start().await.unwrap();
        let id = scheduler
            .add_one_shot_job(Duration::from_secs(3600), "test-remove", || {
                Box::pin(async {})
            })
            .await
            .unwrap();
        // Should not error even if job hasn't fired
        scheduler.remove_job(id).await.unwrap();
    }
}
