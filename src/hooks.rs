use anyhow::Result;
use tracing::{debug, warn};

/// Context passed to every lifecycle hook invocation.
///
/// Fields are populated based on the hook stage — for example `tool_name` is
/// only present for `pre_tool_call` / `post_tool_call`, and `tool_result` is
/// only present for `post_tool_call`.
#[derive(Debug, Clone)]
pub struct HookContext {
    /// Telegram (or other platform) user identifier.
    pub user_id: String,
    /// Chat / conversation identifier.
    pub chat_id: String,
    /// The user message text that initiated this agent loop (if available).
    pub message: Option<String>,
    /// Name of the tool being called (set for tool-related hooks).
    pub tool_name: Option<String>,
    /// Arguments passed to the tool (set for `pre_tool_call` / `post_tool_call`).
    pub tool_args: Option<serde_json::Value>,
    /// Result returned by the tool (set for `post_tool_call`).
    pub tool_result: Option<String>,
    /// Current agentic-loop iteration (0-based).
    pub iteration: u32,
    /// Extensible key-value metadata for custom hooks.
    #[allow(dead_code)]
    pub metadata: serde_json::Value,
}

impl HookContext {
    /// Create a minimal context for the start of a request.
    pub fn new(user_id: &str, chat_id: &str) -> Self {
        Self {
            user_id: user_id.to_string(),
            chat_id: chat_id.to_string(),
            message: None,
            tool_name: None,
            tool_args: None,
            tool_result: None,
            iteration: 0,
            metadata: serde_json::Value::Object(serde_json::Map::new()),
        }
    }

    /// Builder-style setter for message.
    pub fn with_message(mut self, msg: &str) -> Self {
        self.message = Some(msg.to_string());
        self
    }

    /// Builder-style setter for tool name.
    pub fn with_tool_name(mut self, name: &str) -> Self {
        self.tool_name = Some(name.to_string());
        self
    }

    /// Builder-style setter for tool arguments.
    pub fn with_tool_args(mut self, args: &serde_json::Value) -> Self {
        self.tool_args = Some(args.clone());
        self
    }

    /// Builder-style setter for tool result.
    pub fn with_tool_result(mut self, result: &str) -> Self {
        self.tool_result = Some(result.to_string());
        self
    }

    /// Builder-style setter for iteration.
    pub fn with_iteration(mut self, iteration: u32) -> Self {
        self.iteration = iteration;
        self
    }

    /// Builder-style setter for arbitrary metadata.
    #[allow(dead_code)]
    pub fn with_metadata(mut self, key: &str, value: serde_json::Value) -> Self {
        if let serde_json::Value::Object(ref mut map) = self.metadata {
            map.insert(key.to_string(), value);
        }
        self
    }
}

/// 10-stage lifecycle hook trait for the agent loop.
///
/// All methods have default no-op implementations so that concrete hooks only
/// need to override the stages they care about.
#[async_trait::async_trait]
pub trait AgentHook: Send + Sync {
    /// Called once at the start of `process_message`, before the agentic loop.
    async fn pre_process(&self, _ctx: &HookContext) -> Result<()> {
        Ok(())
    }

    /// Called before each LLM API call inside the loop.
    async fn pre_llm_call(&self, _ctx: &HookContext) -> Result<()> {
        Ok(())
    }

    /// Called after each LLM API response is received.
    async fn post_llm_call(&self, _ctx: &HookContext) -> Result<()> {
        Ok(())
    }

    /// Called before executing each individual tool call.
    async fn pre_tool_call(&self, _ctx: &HookContext) -> Result<()> {
        Ok(())
    }

    /// Called after a tool execution completes (success or error).
    async fn post_tool_call(&self, _ctx: &HookContext) -> Result<()> {
        Ok(())
    }

    /// Called before invoking a subagent via `run_subagent`.
    async fn pre_subagent(&self, _ctx: &HookContext) -> Result<()> {
        Ok(())
    }

    /// Called after a subagent invocation returns.
    async fn post_subagent(&self, _ctx: &HookContext) -> Result<()> {
        Ok(())
    }

    /// Called before saving a message to persistent memory.
    async fn pre_save_message(&self, _ctx: &HookContext) -> Result<()> {
        Ok(())
    }

    /// Called after saving a message to persistent memory.
    async fn post_save_message(&self, _ctx: &HookContext) -> Result<()> {
        Ok(())
    }

    /// Called when an error occurs during the agent loop.
    async fn on_error(&self, _ctx: &HookContext, _error: &str) -> Result<()> {
        Ok(())
    }
}

/// Manages a collection of [`AgentHook`] implementations and dispatches
/// lifecycle events to all registered hooks in order.
pub struct HookManager {
    hooks: Vec<Box<dyn AgentHook>>,
}

impl HookManager {
    /// Create an empty hook manager (no hooks registered).
    pub fn new() -> Self {
        Self { hooks: Vec::new() }
    }

    /// Register a new hook. Hooks are called in registration order.
    pub fn register(&mut self, hook: Box<dyn AgentHook>) {
        self.hooks.push(hook);
    }

    // ── Dispatch helpers ────────────────────────────────────────────

    pub async fn run_pre_process(&self, ctx: &HookContext) {
        for hook in &self.hooks {
            if let Err(e) = hook.pre_process(ctx).await {
                warn!("Hook pre_process error: {:#}", e);
            }
        }
    }

    pub async fn run_pre_llm_call(&self, ctx: &HookContext) {
        for hook in &self.hooks {
            if let Err(e) = hook.pre_llm_call(ctx).await {
                warn!("Hook pre_llm_call error: {:#}", e);
            }
        }
    }

    pub async fn run_post_llm_call(&self, ctx: &HookContext) {
        for hook in &self.hooks {
            if let Err(e) = hook.post_llm_call(ctx).await {
                warn!("Hook post_llm_call error: {:#}", e);
            }
        }
    }

    pub async fn run_pre_tool_call(&self, ctx: &HookContext) {
        for hook in &self.hooks {
            if let Err(e) = hook.pre_tool_call(ctx).await {
                warn!("Hook pre_tool_call error: {:#}", e);
            }
        }
    }

    pub async fn run_post_tool_call(&self, ctx: &HookContext) {
        for hook in &self.hooks {
            if let Err(e) = hook.post_tool_call(ctx).await {
                warn!("Hook post_tool_call error: {:#}", e);
            }
        }
    }

    pub async fn run_pre_subagent(&self, ctx: &HookContext) {
        for hook in &self.hooks {
            if let Err(e) = hook.pre_subagent(ctx).await {
                warn!("Hook pre_subagent error: {:#}", e);
            }
        }
    }

    pub async fn run_post_subagent(&self, ctx: &HookContext) {
        for hook in &self.hooks {
            if let Err(e) = hook.post_subagent(ctx).await {
                warn!("Hook post_subagent error: {:#}", e);
            }
        }
    }

    pub async fn run_pre_save_message(&self, ctx: &HookContext) {
        for hook in &self.hooks {
            if let Err(e) = hook.pre_save_message(ctx).await {
                warn!("Hook pre_save_message error: {:#}", e);
            }
        }
    }

    pub async fn run_post_save_message(&self, ctx: &HookContext) {
        for hook in &self.hooks {
            if let Err(e) = hook.post_save_message(ctx).await {
                warn!("Hook post_save_message error: {:#}", e);
            }
        }
    }

    pub async fn run_on_error(&self, ctx: &HookContext, error: &str) {
        for hook in &self.hooks {
            if let Err(e) = hook.on_error(ctx, error).await {
                warn!("Hook on_error error: {:#}", e);
            }
        }
    }
}

// ── Built-in hook: LoggingHook ──────────────────────────────────────────

/// A simple hook that logs every lifecycle event at `debug` level.
/// Useful for development and diagnostics.
pub struct LoggingHook;

#[async_trait::async_trait]
impl AgentHook for LoggingHook {
    async fn pre_process(&self, ctx: &HookContext) -> Result<()> {
        debug!(
            user_id = %ctx.user_id,
            chat_id = %ctx.chat_id,
            message = ?ctx.message,
            "[hook] pre_process"
        );
        Ok(())
    }

    async fn pre_llm_call(&self, ctx: &HookContext) -> Result<()> {
        debug!(
            user_id = %ctx.user_id,
            iteration = ctx.iteration,
            "[hook] pre_llm_call"
        );
        Ok(())
    }

    async fn post_llm_call(&self, ctx: &HookContext) -> Result<()> {
        debug!(
            user_id = %ctx.user_id,
            iteration = ctx.iteration,
            "[hook] post_llm_call"
        );
        Ok(())
    }

    async fn pre_tool_call(&self, ctx: &HookContext) -> Result<()> {
        debug!(
            user_id = %ctx.user_id,
            tool = ?ctx.tool_name,
            "[hook] pre_tool_call"
        );
        Ok(())
    }

    async fn post_tool_call(&self, ctx: &HookContext) -> Result<()> {
        debug!(
            user_id = %ctx.user_id,
            tool = ?ctx.tool_name,
            result_len = ctx.tool_result.as_ref().map(|r| r.len()),
            "[hook] post_tool_call"
        );
        Ok(())
    }

    async fn pre_subagent(&self, ctx: &HookContext) -> Result<()> {
        debug!(
            user_id = %ctx.user_id,
            tool = ?ctx.tool_name,
            "[hook] pre_subagent"
        );
        Ok(())
    }

    async fn post_subagent(&self, ctx: &HookContext) -> Result<()> {
        debug!(
            user_id = %ctx.user_id,
            tool = ?ctx.tool_name,
            "[hook] post_subagent"
        );
        Ok(())
    }

    async fn pre_save_message(&self, ctx: &HookContext) -> Result<()> {
        debug!(
            user_id = %ctx.user_id,
            "[hook] pre_save_message"
        );
        Ok(())
    }

    async fn post_save_message(&self, ctx: &HookContext) -> Result<()> {
        debug!(
            user_id = %ctx.user_id,
            "[hook] post_save_message"
        );
        Ok(())
    }

    async fn on_error(&self, ctx: &HookContext, error: &str) -> Result<()> {
        debug!(
            user_id = %ctx.user_id,
            error = %error,
            "[hook] on_error"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A test hook that counts how many times each stage is called.
    struct CountingHook {
        pre_process_count: AtomicU32,
        pre_llm_call_count: AtomicU32,
        post_tool_call_count: AtomicU32,
        on_error_count: AtomicU32,
    }

    impl CountingHook {
        fn new() -> Self {
            Self {
                pre_process_count: AtomicU32::new(0),
                pre_llm_call_count: AtomicU32::new(0),
                post_tool_call_count: AtomicU32::new(0),
                on_error_count: AtomicU32::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl AgentHook for CountingHook {
        async fn pre_process(&self, _ctx: &HookContext) -> Result<()> {
            self.pre_process_count.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        async fn pre_llm_call(&self, _ctx: &HookContext) -> Result<()> {
            self.pre_llm_call_count.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        async fn post_tool_call(&self, _ctx: &HookContext) -> Result<()> {
            self.post_tool_call_count.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        async fn on_error(&self, _ctx: &HookContext, _error: &str) -> Result<()> {
            self.on_error_count.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    #[test]
    fn test_hook_context_builder() {
        let ctx = HookContext::new("user1", "chat1")
            .with_message("hello")
            .with_tool_name("read_file")
            .with_iteration(3)
            .with_metadata("key", serde_json::json!("value"));

        assert_eq!(ctx.user_id, "user1");
        assert_eq!(ctx.chat_id, "chat1");
        assert_eq!(ctx.message.as_deref(), Some("hello"));
        assert_eq!(ctx.tool_name.as_deref(), Some("read_file"));
        assert_eq!(ctx.iteration, 3);
        assert_eq!(ctx.metadata["key"], "value");
    }

    #[tokio::test]
    async fn test_hook_manager_dispatches_to_all_hooks() {
        let hook = std::sync::Arc::new(CountingHook::new());
        let hook_clone = std::sync::Arc::clone(&hook);

        let mut manager = HookManager::new();
        // We need to wrap Arc<CountingHook> to satisfy Box<dyn AgentHook>.
        // Use a forwarding wrapper.
        struct ArcHook(std::sync::Arc<CountingHook>);

        #[async_trait::async_trait]
        impl AgentHook for ArcHook {
            async fn pre_process(&self, ctx: &HookContext) -> Result<()> {
                self.0.pre_process(ctx).await
            }
            async fn pre_llm_call(&self, ctx: &HookContext) -> Result<()> {
                self.0.pre_llm_call(ctx).await
            }
            async fn post_tool_call(&self, ctx: &HookContext) -> Result<()> {
                self.0.post_tool_call(ctx).await
            }
            async fn on_error(&self, ctx: &HookContext, error: &str) -> Result<()> {
                self.0.on_error(ctx, error).await
            }
        }

        manager.register(Box::new(ArcHook(hook_clone)));

        let ctx = HookContext::new("u", "c");

        manager.run_pre_process(&ctx).await;
        manager.run_pre_process(&ctx).await;
        manager.run_pre_llm_call(&ctx).await;
        manager.run_post_tool_call(&ctx).await;
        manager.run_on_error(&ctx, "test error").await;

        assert_eq!(hook.pre_process_count.load(Ordering::Relaxed), 2);
        assert_eq!(hook.pre_llm_call_count.load(Ordering::Relaxed), 1);
        assert_eq!(hook.post_tool_call_count.load(Ordering::Relaxed), 1);
        assert_eq!(hook.on_error_count.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn test_hook_errors_are_non_fatal() {
        struct FailingHook;

        #[async_trait::async_trait]
        impl AgentHook for FailingHook {
            async fn pre_process(&self, _ctx: &HookContext) -> Result<()> {
                anyhow::bail!("intentional failure")
            }
        }

        let mut manager = HookManager::new();
        manager.register(Box::new(FailingHook));

        let ctx = HookContext::new("u", "c");
        // Should not panic — errors are logged but swallowed.
        manager.run_pre_process(&ctx).await;
    }

    #[tokio::test]
    async fn test_empty_hook_manager_is_noop() {
        let manager = HookManager::new();
        let ctx = HookContext::new("u", "c");

        // All dispatches should complete without error.
        manager.run_pre_process(&ctx).await;
        manager.run_pre_llm_call(&ctx).await;
        manager.run_post_llm_call(&ctx).await;
        manager.run_pre_tool_call(&ctx).await;
        manager.run_post_tool_call(&ctx).await;
        manager.run_pre_subagent(&ctx).await;
        manager.run_post_subagent(&ctx).await;
        manager.run_pre_save_message(&ctx).await;
        manager.run_post_save_message(&ctx).await;
        manager.run_on_error(&ctx, "no hooks").await;
    }
}
