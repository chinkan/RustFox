use tracing::info;

use crate::memory::MemoryStore;
use crate::scheduler::Scheduler;

/// Register built-in background tasks
pub async fn register_builtin_tasks(
    scheduler: &Scheduler,
    _memory: MemoryStore,
    llm: crate::llm::LlmClient,
    summarize_cron: String,
    summarize_threshold: usize,
    user_model_cron: String,
    home: std::path::PathBuf,
) -> anyhow::Result<()> {
    // Heartbeat — log that the bot is alive every hour
    scheduler
        .add_cron_job("0 0 * * * *", "heartbeat", || {
            Box::pin(async {
                info!("Heartbeat: bot is alive");
            })
        })
        .await?;

    // Nightly conversation summarization
    {
        let memory_clone = _memory.clone();
        let llm_clone = llm.clone();
        scheduler
            .add_cron_job(&summarize_cron, "nightly-summarization", move || {
                let store = memory_clone.clone();
                let llm = llm_clone.clone();
                let threshold = summarize_threshold;
                Box::pin(async move {
                    if let Err(e) =
                        crate::memory::summarizer::summarize_all_active(&store, &llm, threshold)
                            .await
                    {
                        tracing::error!("Nightly summarization failed: {:#}", e);
                    }
                })
            })
            .await?;
    }

    // Weekly user model update
    {
        let memory_clone = _memory.clone();
        let llm_clone = llm.clone();
        let home_path = home.clone();
        scheduler
            .add_cron_job(&user_model_cron, "weekly-user-model-update", move || {
                let store = memory_clone.clone();
                let llm = llm_clone.clone();
                let path = home_path.join("USER.md");
                let home = home_path.clone();
                Box::pin(async move {
                    // Check if any soul file was updated in the last 24h
                    let recent_update = ["SOUL.md", "AGENTS.md", "USER.md"].iter().any(|name| {
                        let p = home.join(name);
                        if let Ok(meta) = std::fs::metadata(&p) {
                            if let Ok(modified) = meta.modified() {
                                if let Ok(duration) = modified.elapsed() {
                                    return duration < std::time::Duration::from_secs(86400);
                                }
                            }
                        }
                        false
                    });

                    if recent_update {
                        tracing::info!(
                            "Soul files recently updated — skipping cron user model update"
                        );
                        return;
                    }
                    crate::learning::update_user_model(&llm, &store, &path).await;
                })
            })
            .await?;
    }

    Ok(())
}
