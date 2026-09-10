use anyhow::Context;
use async_trait::async_trait;
use serde_json::{json, Value};

use crate::llm::{FunctionDefinition, ToolDefinition};
use crate::memory::knowledge::FactToAdd;
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
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "add_fact".to_string(),
                    description: "Record a temporal fact (entity, relation, value) with validity start. Replaces any prior active value for the same entity+relation.".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "entity": { "type": "string", "description": "Subject entity (e.g. 'Kan')" },
                            "relation": { "type": "string", "description": "Relation (e.g. 'prefers')" },
                            "value": { "type": "string", "description": "Object value (e.g. 'Nike')" },
                            "valid_from": { "type": "string", "description": "ISO date when fact became true (e.g. '2024-09-01')" },
                            "source": { "type": "string", "description": "Optional provenance" },
                            "confidence": { "type": "number", "description": "0.0-1.0, default 1.0" }
                        },
                        "required": ["entity", "relation", "value", "valid_from"]
                    }),
                },
            },
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "query_facts".to_string(),
                    description: "Get facts for an entity. Omit as_of for currently active facts; pass a date for a point-in-time snapshot.".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "entity": { "type": "string" },
                            "as_of": { "type": "string", "description": "Optional ISO date snapshot" }
                        },
                        "required": ["entity"]
                    }),
                },
            },
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "close_fact".to_string(),
                    description: "End the currently-active fact for an entity+relation at valid_to.".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "entity": { "type": "string" },
                            "relation": { "type": "string" },
                            "valid_to": { "type": "string", "description": "ISO date when fact stopped being true" }
                        },
                        "required": ["entity", "relation", "valid_to"]
                    }),
                },
            },
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "fact_history".to_string(),
                    description: "Version timeline for knowledge (category+key) or facts (entity+relation). With category+key+as_of, returns knowledge value at that time.".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "category": { "type": "string", "description": "Knowledge category" },
                            "key": { "type": "string", "description": "Knowledge key" },
                            "entity": { "type": "string", "description": "Fact entity" },
                            "relation": { "type": "string", "description": "Fact relation" },
                            "as_of": { "type": "string", "description": "Optional ISO date; with category+key returns knowledge_as_of" }
                        }
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
                        results.push(format!(
                            "[{}]: {}",
                            msg.role,
                            msg.content
                                .as_ref()
                                .map(|c| c.as_text())
                                .unwrap_or_default()
                        ));
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
            "add_fact" => {
                let entity = args["entity"].as_str().context("Missing entity")?;
                let relation = args["relation"].as_str().context("Missing relation")?;
                let value = args["value"].as_str().context("Missing value")?;
                let valid_from = args["valid_from"].as_str().context("Missing valid_from")?;
                let source = args["source"].as_str().map(|s| s.to_string());
                let confidence = args["confidence"].as_f64();
                match self
                    .memory
                    .add_fact(FactToAdd {
                        entity: entity.into(),
                        relation: relation.into(),
                        value: value.into(),
                        valid_from: valid_from.into(),
                        source,
                        confidence,
                    })
                    .await
                {
                    Ok(id) => Ok(format!(
                        "Fact {id}: {entity} —{relation}→ {value} (from {valid_from})"
                    )),
                    Err(e) => Ok(format!("Failed to add fact: {e}")),
                }
            }
            "query_facts" => {
                let entity = args["entity"].as_str().context("Missing entity")?;
                let as_of = args["as_of"].as_str();
                match self.memory.query_facts(entity, as_of).await {
                    Ok(facts) if facts.is_empty() => Ok("No facts found.".into()),
                    Ok(facts) => Ok(facts
                        .iter()
                        .map(|f| {
                            let until = f.valid_to.as_deref().unwrap_or("…");
                            format!(
                                "{} —{}→ {} [{}..{}] conf={}",
                                f.entity, f.relation, f.value, f.valid_from, until, f.confidence
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")),
                    Err(e) => Ok(format!("Failed to query facts: {e}")),
                }
            }
            "close_fact" => {
                let entity = args["entity"].as_str().context("Missing entity")?;
                let relation = args["relation"].as_str().context("Missing relation")?;
                let valid_to = args["valid_to"].as_str().context("Missing valid_to")?;
                match self.memory.close_fact(entity, relation, valid_to).await {
                    Ok(true) => Ok(format!("Closed {entity}.{relation} at {valid_to}")),
                    Ok(false) => Ok("No active fact to close.".into()),
                    Err(e) => Ok(format!("Failed to close fact: {e}")),
                }
            }
            "fact_history" => {
                let category = args["category"].as_str();
                let key = args["key"].as_str();
                let entity = args["entity"].as_str();
                let relation = args["relation"].as_str();
                let as_of = args["as_of"].as_str();
                if let (Some(cat), Some(k), Some(as_of)) = (category, key, as_of) {
                    match self.memory.knowledge_as_of(cat, k, as_of).await {
                        Ok(Some(v)) => Ok(format!("[{cat}/{k}] as of {as_of}: {v}")),
                        Ok(None) => Ok(format!("No value for [{cat}/{k}] as of {as_of}")),
                        Err(e) => Ok(format!("Failed knowledge_as_of: {e}")),
                    }
                } else if let (Some(cat), Some(k)) = (category, key) {
                    match self.memory.knowledge_timeline(cat, k).await {
                        Ok(versions) if versions.is_empty() => Ok("No knowledge history.".into()),
                        Ok(versions) => Ok(versions
                            .iter()
                            .map(|v| {
                                format!(
                                    "[{}] {} → {} ({})",
                                    v.changed_at,
                                    v.old_value.as_deref().unwrap_or("∅"),
                                    v.new_value.as_deref().unwrap_or("∅"),
                                    v.change_type
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("\n")),
                        Err(e) => Ok(format!("Failed knowledge history: {e}")),
                    }
                } else if let (Some(ent), Some(rel)) = (entity, relation) {
                    match self.memory.fact_timeline(ent, rel).await {
                        Ok(facts) if facts.is_empty() => Ok("No fact history.".into()),
                        Ok(facts) => Ok(facts
                            .iter()
                            .map(|f| {
                                let until = f.valid_to.as_deref().unwrap_or("…");
                                format!(
                                    "{} —{}→ {} [{}..{}]",
                                    f.entity, f.relation, f.value, f.valid_from, until
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("\n")),
                        Err(e) => Ok(format!("Failed fact history: {e}")),
                    }
                } else {
                    Ok("Provide category+key (knowledge) or entity+relation (facts).".into())
                }
            }
            _ => anyhow::bail!("MemoryTools: unknown tool {name}"),
        }
    }
}
