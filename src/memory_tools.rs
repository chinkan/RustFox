use anyhow::Context;
use async_trait::async_trait;
use serde_json::{json, Value};

use crate::llm::{FunctionDefinition, ToolDefinition};
use crate::memory::MemoryStore;
use crate::tool_registry::{ToolContext, ToolHandler, ToolResult};

pub struct MemoryTools {
    memory: MemoryStore,
}

impl MemoryTools {
    pub fn new(memory: MemoryStore) -> Self {
        Self { memory }
    }
}

#[async_trait]
impl ToolHandler for MemoryTools {
    fn define(&self) -> Vec<ToolDefinition> {
        vec![
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "remember".to_string(),
                    description: "Store a piece of knowledge for long-term memory. Use this to remember user preferences, facts, or anything useful.".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "category": { "type": "string", "description": "Category (e.g., 'user_preference', 'fact', 'project')" },
                            "key": { "type": "string", "description": "Short identifier for this knowledge" },
                            "value": { "type": "string", "description": "The knowledge to remember" }
                        },
                        "required": ["category", "key", "value"]
                    }),
                },
            },
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "recall".to_string(),
                    description: "Retrieve a specific piece of remembered knowledge.".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "category": { "type": "string", "description": "Category to search in" },
                            "key": { "type": "string", "description": "The key to look up" }
                        },
                        "required": ["category", "key"]
                    }),
                },
            },
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "search_memory".to_string(),
                    description: "Search through past conversations and knowledge using hybrid vector + full-text search. Finds semantically similar content even with different wording.".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "query": { "type": "string", "description": "Search query (natural language)" },
                            "limit": { "type": "integer", "description": "Max results (default 5)" }
                        },
                        "required": ["query"]
                    }),
                },
            },
        ]
    }

    async fn execute(&self, name: &str, args: Value, _ctx: ToolContext) -> ToolResult {
        match name {
            "remember" => {
                let category = args["category"].as_str().unwrap_or("general");
                let key = args["key"].as_str().unwrap_or("");
                let value = args["value"].as_str().unwrap_or("");
                match self.memory.remember(category, key, value, None).await {
                    Ok(()) => Ok(format!("Remembered: [{}] {} = {}", category, key, value)),
                    Err(e) => Ok(format!("Failed to remember: {}", e)),
                }
            }
            "recall" => {
                let category = args["category"].as_str().unwrap_or("general");
                let key = args["key"].as_str().unwrap_or("");
                match self.memory.recall(category, key).await {
                    Ok(Some(value)) => Ok(value),
                    Ok(None) => Ok(format!("No knowledge found for [{}] {}", category, key)),
                    Err(e) => Ok(format!("Failed to recall: {}", e)),
                }
            }
            "search_memory" => {
                let query = args["query"].as_str().context("Missing 'query' argument")?;
                let limit = args["limit"].as_u64().unwrap_or(5) as usize;

                let mut results = Vec::new();

                if let Ok(msgs) = self.memory.search_messages(query, limit).await {
                    for msg in msgs {
                        results.push(format!("[{}]: {}", msg.role, msg.content.as_ref().map(|c| c.as_text()).unwrap_or_default()));
                    }
                }

                if let Ok(entries) = self.memory.search_knowledge(query, limit).await {
                    for entry in entries {
                        results.push(format!(
                            "[knowledge:{}] {} = {}",
                            entry.category, entry.key, entry.value
                        ));
                    }
                }

                if results.is_empty() {
                    Ok("No results found.".to_string())
                } else {
                    Ok(results.join("\n\n"))
                }
            }
            _ => anyhow::bail!("MemoryTools: unknown tool {name}"),
        }
    }
}
