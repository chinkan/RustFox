use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;

use crate::cancel_registry::CancelRegistry;
use crate::llm::ToolDefinition;
use crate::platform::sender::PlatformSender;

pub type ToolResult = Result<String>;

pub struct ToolContext {
    pub sandbox_dir: PathBuf,
    pub home_dir: Option<PathBuf>,
    pub sender: Arc<dyn PlatformSender>,
    pub cancel_registry: Arc<CancelRegistry>,
    pub user_id: String,
    pub chat_id: String,
}

#[async_trait]
pub trait ToolHandler: Send + Sync {
    fn define(&self) -> Vec<ToolDefinition>;
    async fn execute(&self, name: &str, args: Value, ctx: ToolContext) -> ToolResult;
}

pub struct ToolRegistry {
    handlers: Vec<Box<dyn ToolHandler>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            handlers: Vec::new(),
        }
    }

    pub fn register(&mut self, handler: Box<dyn ToolHandler>) {
        self.handlers.push(handler);
    }

    pub fn all_definitions(&self) -> Vec<ToolDefinition> {
        let mut all = Vec::new();
        for handler in &self.handlers {
            all.extend(handler.define());
        }
        all
    }

    pub async fn execute(&self, name: &str, args: Value, ctx: ToolContext) -> ToolResult {
        for handler in &self.handlers {
            if handler.define().iter().any(|d| d.function.name == name) {
                return handler.execute(name, args, ctx).await;
            }
        }
        anyhow::bail!("Unknown tool: {name}")
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::FunctionDefinition;
    use crate::platform::sender::{MessageFormat, PlatformMessageId};
    use serde_json::json;
    use std::path::Path;
    use std::sync::Arc;

    struct MockHandler;

    #[async_trait]
    impl ToolHandler for MockHandler {
        fn define(&self) -> Vec<ToolDefinition> {
            vec![ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "mock_tool".to_string(),
                    description: "A mock tool".to_string(),
                    parameters: json!({ "type": "object", "properties": {} }),
                },
            }]
        }

        async fn execute(&self, name: &str, _args: Value, _ctx: ToolContext) -> ToolResult {
            Ok(format!("executed {name}"))
        }
    }

    #[tokio::test]
    async fn test_register_and_execute() {
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(MockHandler));
        let defs = reg.all_definitions();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].function.name, "mock_tool");

        let ctx = ToolContext {
            sandbox_dir: PathBuf::from("/tmp"),
            home_dir: None,
            sender: Arc::new(TestSender),
            cancel_registry: Arc::new(CancelRegistry::new()),
            user_id: "test".to_string(),
            chat_id: "0".to_string(),
        };
        let result = reg.execute("mock_tool", json!({}), ctx).await.unwrap();
        assert_eq!(result, "executed mock_tool");
    }

    #[tokio::test]
    async fn test_unknown_tool() {
        let reg = ToolRegistry::new();
        let ctx = ToolContext {
            sandbox_dir: PathBuf::from("/tmp"),
            home_dir: None,
            sender: Arc::new(TestSender),
            cancel_registry: Arc::new(CancelRegistry::new()),
            user_id: "test".to_string(),
            chat_id: "0".to_string(),
        };
        let result = reg.execute("unknown", json!({}), ctx).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Unknown tool"));
    }

    struct TestSender;

    #[async_trait]
    impl PlatformSender for TestSender {
        async fn send_message(
            &self,
            _chat_id: &str,
            _text: &str,
            _format: MessageFormat,
        ) -> Result<PlatformMessageId> {
            Ok("test:1".to_string())
        }
        async fn send_file(
            &self,
            _chat_id: &str,
            _path: &Path,
            _caption: Option<&str>,
        ) -> Result<PlatformMessageId> {
            Ok("test:1".to_string())
        }
        async fn show_cancel_button(
            &self,
            _chat_id: &str,
            _text: &str,
            _cancel_id: &str,
        ) -> Result<PlatformMessageId> {
            Ok("test:1".to_string())
        }
        async fn edit_message(
            &self,
            _chat_id: &str,
            _message_id: &PlatformMessageId,
            _text: &str,
        ) -> Result<()> {
            Ok(())
        }
        async fn notify_shutdown(&self, _chat_id: &str) -> Result<()> {
            Ok(())
        }
    }
}
