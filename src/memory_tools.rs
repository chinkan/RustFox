use anyhow::Context;
use async_trait::async_trait;
use serde_json::{json, Value};

use crate::llm::{FunctionDefinition, ToolDefinition};
use crate::memory::evidence::{format_evidence_suffix, parse_evidence_value, StoredEvidence};
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
                    description: "Store a piece of knowledge for long-term memory. Pass evidence when you know where it came from: one object {kind, ref, captured_at} or an array of those. kind is run, conversation, file, url, dream, or unknown. ref is the pointer (run id, conversation and message id, file path, URL, dream file, or source knowledge id). captured_at is an ISO-8601 time when you have one. If you omit evidence, the row is stored as unknown and recall/search show it as untrusted (⚠️ 無來源). Omitting evidence replaces any previous pointer.".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "category": { "type": "string", "description": "Category (e.g., 'user_preference', 'fact', 'project')" },
                            "key": { "type": "string", "description": "Short identifier for this knowledge" },
                            "value": { "type": "string", "description": "The knowledge to remember" },
                            "evidence": {
                                "description": "Provenance pointer. One object or an array. Omit only when you have no source; that stores kind unknown and is not trusted.",
                                "anyOf": [
                                    {
                                        "type": "object",
                                        "properties": {
                                            "kind": { "type": "string", "enum": ["run", "conversation", "file", "url", "dream", "unknown"] },
                                            "ref": { "type": "string", "description": "Pointer for this kind. Required except for unknown." },
                                            "captured_at": { "type": "string", "description": "ISO-8601 time the pointer was captured, when known." }
                                        },
                                        "required": ["kind"]
                                    },
                                    {
                                        "type": "array",
                                        "items": {
                                            "type": "object",
                                            "properties": {
                                                "kind": { "type": "string", "enum": ["run", "conversation", "file", "url", "dream", "unknown"] },
                                                "ref": { "type": "string" },
                                                "captured_at": { "type": "string" }
                                            },
                                            "required": ["kind"]
                                        }
                                    },
                                    { "type": "string", "description": "JSON text of one pointer or an array of pointers." }
                                ]
                            }
                        },
                        "required": ["category", "key", "value"]
                    }),
                },
            },
            ToolDefinition {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: "recall".to_string(),
                    description: "Retrieve a specific piece of remembered knowledge, including its evidence. Unknown or missing evidence is shown as ⚠️ 無來源 and is not trusted.".to_string(),
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
                    description: "Search through past conversations and knowledge using hybrid vector + full-text search. Knowledge hits include evidence. Unknown or missing evidence is shown as ⚠️ 無來源 and is not trusted.".to_string(),
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
                let evidence = match evidence_from_args(&args) {
                    Ok(evidence) => evidence,
                    Err(e) => return Ok(format!("Failed to remember: {e}")),
                };
                match self
                    .memory
                    .remember_with_evidence(category, key, value, None, evidence.as_ref())
                    .await
                {
                    Ok(()) => Ok(format!("Remembered: [{}] {} = {}", category, key, value)),
                    Err(e) => Ok(format!("Failed to remember: {}", e)),
                }
            }
            "recall" => {
                let category = args["category"].as_str().unwrap_or("general");
                let key = args["key"].as_str().unwrap_or("");
                match self.memory.recall_entry(category, key).await {
                    Ok(Some(entry)) => Ok(format!(
                        "{}\n\n{}",
                        entry.value,
                        format_evidence_suffix(entry.evidence.as_deref())
                    )),
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
                            "[knowledge:{}] {} = {}\n{}",
                            entry.category,
                            entry.key,
                            entry.value,
                            format_evidence_suffix(entry.evidence.as_deref())
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

/// Missing or JSON null evidence is omitted, which the store writes as unknown.
fn evidence_from_args(args: &Value) -> anyhow::Result<Option<StoredEvidence>> {
    match args.get("evidence") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => parse_evidence_value(value).map(Some),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cancel_registry::CancelRegistry;
    use crate::memory::evidence::{
        EvidenceKind, EvidencePointer, StoredEvidence, UNTRUSTED_EVIDENCE_MARK,
    };
    use crate::memory::MemoryStore;
    use crate::platform::sender::{MessageFormat, PlatformMessageId, PlatformSender};
    use crate::tool_registry::{ToolContext, ToolUiMode};
    use async_trait::async_trait;
    use serde_json::json;
    use std::path::Path;
    use std::sync::Arc;

    struct TestSender;

    #[async_trait]
    impl PlatformSender for TestSender {
        async fn send_message(
            &self,
            _chat_id: &str,
            _text: &str,
            _format: MessageFormat,
        ) -> anyhow::Result<PlatformMessageId> {
            Ok("test:1".into())
        }
        async fn send_file(
            &self,
            _chat_id: &str,
            _path: &Path,
            _caption: Option<&str>,
        ) -> anyhow::Result<PlatformMessageId> {
            Ok("test:1".into())
        }
        async fn show_cancel_button(
            &self,
            _chat_id: &str,
            _text: &str,
            _cancel_id: &str,
        ) -> anyhow::Result<PlatformMessageId> {
            Ok("test:1".into())
        }
        async fn edit_message(
            &self,
            _chat_id: &str,
            _message_id: &PlatformMessageId,
            _text: &str,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn delete_message(
            &self,
            _chat_id: &str,
            _message_id: &PlatformMessageId,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn notify_shutdown(&self, _chat_id: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn ctx() -> ToolContext {
        ToolContext {
            sandbox_dir: Path::new("/tmp").into(),
            home_dir: None,
            sender: Arc::new(TestSender),
            cancel_registry: Arc::new(CancelRegistry::new()),
            user_id: "user".into(),
            chat_id: "chat".into(),
            bot_id: crate::platform::DEFAULT_BOT_ID.into(),
            tool_ui_mode: ToolUiMode::Minimal,
        }
    }

    fn harness() -> (MemoryTools, MemoryStore) {
        let memory = MemoryStore::open_in_memory().unwrap();
        let tools = MemoryTools::new(memory.clone());
        (tools, memory)
    }

    #[test]
    fn remember_schema_has_optional_evidence() {
        let (tools, _) = harness();
        let def = tools
            .define()
            .into_iter()
            .find(|tool| tool.function.name == "remember")
            .unwrap();
        let required = def.function.parameters["required"].as_array().unwrap();
        assert!(required.iter().any(|name| name == "category"));
        assert!(required.iter().all(|name| name != "evidence"));
        let evidence = &def.function.parameters["properties"]["evidence"];
        let text = evidence.to_string();
        for kind in ["run", "conversation", "file", "url", "dream", "unknown"] {
            assert!(text.contains(kind), "schema missing kind {kind}: {text}");
        }
    }

    #[tokio::test]
    async fn omitted_evidence_is_unknown_on_recall_and_search() {
        let (tools, _) = harness();
        tools
            .execute(
                "remember",
                json!({"category": "fact", "key": "city", "value": "betacitytoken lives here"}),
                ctx(),
            )
            .await
            .unwrap();
        let recalled = tools
            .execute("recall", json!({"category": "fact", "key": "city"}), ctx())
            .await
            .unwrap();
        assert!(recalled.contains("betacitytoken lives here"), "{recalled}");
        assert!(recalled.contains(UNTRUSTED_EVIDENCE_MARK), "{recalled}");

        let found = tools
            .execute(
                "search_memory",
                json!({"query": "betacitytoken", "limit": 5}),
                ctx(),
            )
            .await
            .unwrap();
        assert!(
            found.contains("[knowledge:fact] city = betacitytoken lives here"),
            "{found}"
        );
        assert!(found.contains(UNTRUSTED_EVIDENCE_MARK), "{found}");
    }

    #[tokio::test]
    async fn provided_evidence_is_shown_by_recall_and_search() {
        let (tools, _) = harness();
        tools
            .execute(
                "remember",
                json!({
                    "category": "lesson",
                    "key": "ledger",
                    "value": "runledgertoken was learned from a scheduled run",
                    "evidence": {
                        "kind": "run",
                        "ref": "run-42",
                        "captured_at": "2026-11-03T00:00:00Z"
                    }
                }),
                ctx(),
            )
            .await
            .unwrap();
        let recalled = tools
            .execute(
                "recall",
                json!({"category": "lesson", "key": "ledger"}),
                ctx(),
            )
            .await
            .unwrap();
        assert!(recalled.contains("runledgertoken"), "{recalled}");
        assert!(recalled.contains(r#""kind":"run""#), "{recalled}");
        assert!(recalled.contains(r#""ref":"run-42""#), "{recalled}");
        assert!(!recalled.contains(UNTRUSTED_EVIDENCE_MARK), "{recalled}");

        let found = tools
            .execute("search_memory", json!({"query": "runledgertoken"}), ctx())
            .await
            .unwrap();
        assert!(found.contains("[knowledge:lesson]"), "{found}");
        assert!(found.contains(r#""ref":"run-42""#), "{found}");
        assert!(!found.contains(UNTRUSTED_EVIDENCE_MARK), "{found}");
    }

    #[tokio::test]
    async fn evidence_array_and_json_string_are_accepted() {
        let (tools, memory) = harness();
        tools
            .execute(
                "remember",
                json!({
                    "category": "fact",
                    "key": "merged",
                    "value": "mergedtoken from two pointers",
                    "evidence": [
                        {"kind": "dream", "ref": "know-1"},
                        {"kind": "file", "ref": "a.pdf"}
                    ]
                }),
                ctx(),
            )
            .await
            .unwrap();
        let entry = memory
            .recall_entry("fact", "merged")
            .await
            .unwrap()
            .unwrap();
        let parsed: StoredEvidence =
            serde_json::from_str(entry.evidence.unwrap().as_str()).unwrap();
        assert_eq!(
            parsed,
            StoredEvidence::Many(vec![
                EvidencePointer {
                    kind: EvidenceKind::Dream,
                    reference: Some("know-1".into()),
                    captured_at: None,
                },
                EvidencePointer {
                    kind: EvidenceKind::File,
                    reference: Some("a.pdf".into()),
                    captured_at: None,
                },
            ])
        );

        tools
            .execute(
                "remember",
                json!({
                    "category": "fact",
                    "key": "fromstring",
                    "value": "stringtoken",
                    "evidence": r#"{"kind":"url","ref":"https://example.com/paper"}"#
                }),
                ctx(),
            )
            .await
            .unwrap();
        let recalled = tools
            .execute(
                "recall",
                json!({"category": "fact", "key": "fromstring"}),
                ctx(),
            )
            .await
            .unwrap();
        assert!(recalled.contains("https://example.com/paper"), "{recalled}");
        assert!(!recalled.contains(UNTRUSTED_EVIDENCE_MARK), "{recalled}");
    }

    #[tokio::test]
    async fn invalid_evidence_is_rejected_and_does_not_write() {
        let (tools, memory) = harness();
        let err = tools
            .execute(
                "remember",
                json!({
                    "category": "fact",
                    "key": "bad",
                    "value": "should not land",
                    "evidence": {"kind": "guess", "ref": "x"}
                }),
                ctx(),
            )
            .await
            .unwrap();
        assert!(err.contains("Failed to remember"), "{err}");
        assert!(memory.recall("fact", "bad").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn remember_tool_does_not_wipe_an_existing_source() {
        let (tools, memory) = harness();
        let file = StoredEvidence::One(EvidencePointer {
            kind: EvidenceKind::File,
            reference: Some("notes.pdf".into()),
            captured_at: None,
        });
        memory
            .remember_with_evidence(
                "document_chunk",
                "chunk",
                "old body",
                Some("notes.pdf"),
                Some(&file),
            )
            .await
            .unwrap();
        tools
            .execute(
                "remember",
                json!({
                    "category": "document_chunk",
                    "key": "chunk",
                    "value": "new body"
                }),
                ctx(),
            )
            .await
            .unwrap();
        let entry = memory
            .recall_entry("document_chunk", "chunk")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(entry.value, "new body");
        assert_eq!(entry.source.as_deref(), Some("notes.pdf"));
        assert_eq!(entry.evidence.as_deref(), Some(r#"{"kind":"unknown"}"#));
    }
}
