pub mod history;
pub mod reminders;
pub mod reruns;
pub mod schedule;
pub mod tasks;
pub mod timezone;

use anyhow::{Context, Result};
use chrono::Utc;
use chrono_tz::Tz;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio_cron_scheduler::{Job, JobScheduler};
use tracing::info;
use uuid::Uuid;

/// Wrapper around tokio-cron-scheduler for background tasks.
///
/// Recurring jobs are **not** armed with `Job::new_async_tz` (ADR-0022 Decision 7):
/// that API freezes a FixedOffset and uses croner 3. Instead we arm a
/// self-rescheduling one-shot via [`timezone::next_fire`], keeping a stable
/// handle Uuid mapped to the current inner one-shot job id.
pub struct Scheduler {
    inner: JobScheduler,
    /// Stable handle → current inner one-shot Uuid for recurring jobs.
    cron_handles: Arc<Mutex<HashMap<Uuid, Uuid>>>,
}

impl Scheduler {
    pub async fn new() -> Result<Self> {
        let inner = JobScheduler::new()
            .await
            .context("Failed to create job scheduler")?;
        Ok(Self {
            inner,
            cron_handles: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Add a recurring cron job evaluated in `tz`. Returns a stable handle Uuid
    /// (stored as `scheduler_job_id`) that callers use for disarm/edit.
    pub async fn add_cron_job<F>(
        &self,
        cron_expr: &str,
        tz: Tz,
        name: &str,
        task: F,
    ) -> Result<Uuid>
    where
        F: Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
            + Send
            + Sync
            + 'static,
    {
        let handle = Uuid::new_v4();
        let cron_expr = cron_expr.to_string();
        let job_name = name.to_string();
        let task = Arc::new(task);

        let Some(next) = timezone::next_fire(&cron_expr, tz, Utc::now()) else {
            anyhow::bail!("No next fire time for cron '{cron_expr}' in {tz}");
        };
        let delay = (next - Utc::now())
            .to_std()
            .unwrap_or(Duration::from_secs(0));

        let handles = Arc::clone(&self.cron_handles);
        // Reserve the handle so disarm during the first wait is visible.
        handles.lock().await.insert(handle, Uuid::nil());

        let handles_c = Arc::clone(&handles);
        let cron_c = cron_expr.clone();
        let name_c = job_name.clone();
        let task_c = Arc::clone(&task);
        let inner_c = self.inner.clone();

        let job = Job::new_one_shot_async(delay, move |_uuid, _lock| {
            let handles = Arc::clone(&handles_c);
            let cron_expr = cron_c.clone();
            let job_name = name_c.clone();
            let task = Arc::clone(&task_c);
            let inner = inner_c.clone();
            Box::pin(async move {
                run_cron_chain(handle, cron_expr, tz, job_name, task, next, handles, inner).await;
            })
        })
        .with_context(|| format!("Failed to create cron job: {}", job_name))?;

        let id = self
            .inner
            .add(job)
            .await
            .with_context(|| format!("Failed to add job: {}", job_name))?;
        handles.lock().await.insert(handle, id);
        info!(
            "Scheduled task '{}' with cron: {} ({})",
            job_name, cron_expr, tz
        );
        Ok(handle)
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

    /// Remove a job by its UUID (stable handle for recurring, or inner id for one-shot).
    #[allow(dead_code)]
    pub async fn remove_job(&self, id: Uuid) -> Result<()> {
        let inner_id = {
            let mut map = self.cron_handles.lock().await;
            map.remove(&id)
        };
        if let Some(inner) = inner_id {
            if inner != Uuid::nil() {
                let _ = self.inner.remove(&inner).await;
            }
            return Ok(());
        }
        self.inner
            .remove(&id)
            .await
            .with_context(|| format!("Failed to remove job: {}", id))?;
        Ok(())
    }

    /// Whether a job UUID currently has a registered live job.
    #[allow(dead_code)]
    pub async fn is_job_registered(&self, id: Uuid) -> bool {
        let inner_id = {
            let map = self.cron_handles.lock().await;
            map.get(&id).copied()
        };
        let probe = inner_id.unwrap_or(id);
        if probe == Uuid::nil() {
            return inner_id.is_some();
        }
        let mut inner = self.inner.clone();
        matches!(inner.next_tick_for_job(probe).await, Ok(Some(_)))
    }

    pub async fn start(&self) -> Result<()> {
        self.inner
            .start()
            .await
            .context("Failed to start scheduler")?;
        info!("Scheduler started");
        Ok(())
    }

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

/// Run the task at `fired_at`, then chain one-shots until the handle is removed.
#[allow(clippy::too_many_arguments)]
async fn run_cron_chain<F>(
    handle: Uuid,
    cron_expr: String,
    tz: Tz,
    job_name: String,
    task: Arc<F>,
    mut fired_at: chrono::DateTime<Utc>,
    handles: Arc<Mutex<HashMap<Uuid, Uuid>>>,
    inner: JobScheduler,
) where
    F: Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
        + Send
        + Sync
        + 'static,
{
    loop {
        if !handles.lock().await.contains_key(&handle) {
            return;
        }

        info!("Running scheduled task: {}", job_name);
        task().await;

        if !handles.lock().await.contains_key(&handle) {
            return;
        }

        let Some(next) = timezone::next_fire(&cron_expr, tz, fired_at) else {
            tracing::error!(
                "No next fire for recurring task '{}' ({})",
                job_name,
                cron_expr
            );
            return;
        };
        let delay = (next - Utc::now())
            .to_std()
            .unwrap_or(Duration::from_secs(0));

        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let tx = Arc::new(Mutex::new(Some(tx)));
        let tx_c = Arc::clone(&tx);
        let job = match Job::new_one_shot_async(delay, move |_u, _l| {
            let tx_c = Arc::clone(&tx_c);
            Box::pin(async move {
                if let Some(tx) = tx_c.lock().await.take() {
                    let _ = tx.send(());
                }
            })
        }) {
            Ok(j) => j,
            Err(e) => {
                tracing::error!("Failed to create follow-up tick for {job_name}: {e}");
                return;
            }
        };
        let id = match inner.add(job).await {
            Ok(id) => id,
            Err(e) => {
                tracing::error!("Failed to add follow-up tick for {job_name}: {e}");
                return;
            }
        };
        handles.lock().await.insert(handle, id);

        tokio::select! {
            _ = rx => {
                fired_at = next;
            }
            _ = async {
                loop {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    if !handles.lock().await.contains_key(&handle) {
                        break;
                    }
                }
            } => {
                let _ = inner.remove(&id).await;
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_add_cron_job_returns_uuid() {
        let scheduler = Scheduler::new().await.unwrap();
        scheduler.start().await.unwrap();
        let id = scheduler
            .add_cron_job("0 * * * * *", chrono_tz::UTC, "test-cron", || {
                Box::pin(async {})
            })
            .await
            .unwrap();
        assert_ne!(id.as_u128(), 0);
        let _ = scheduler.remove_job(id).await;
    }

    #[tokio::test]
    async fn test_add_one_shot_job() {
        let scheduler = Scheduler::new().await.unwrap();
        scheduler.start().await.unwrap();
        let id = scheduler
            .add_one_shot_job(Duration::from_secs(3600), "test-one", || Box::pin(async {}))
            .await
            .unwrap();
        assert_ne!(id.as_u128(), 0);
        assert!(scheduler.is_job_registered(id).await);
        scheduler.remove_job(id).await.unwrap();
    }
}
