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
    user_model_path: std::path::PathBuf,
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
        let model_path = user_model_path;
        scheduler
            .add_cron_job(&user_model_cron, "weekly-user-model-update", move || {
                let store = memory_clone.clone();
                let llm = llm_clone.clone();
                let path = model_path.clone();
                Box::pin(async move {
                    crate::learning::update_user_model(&llm, &store, &path).await;
                })
            })
            .await?;
    }

    Ok(())
}
