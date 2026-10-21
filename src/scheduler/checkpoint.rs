//! Scheduler timeout transcript checkpoints (ADR-0039 Slice B).
//!
//! Written only on the wall-clock timeout human-gate path. Retry with a
//! checkpoint continues the agent loop from the saved transcript; Cancel
//! clears it. No checkpoint → ADR-0013 full replay.

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::llm::{ChatMessage, MessageContent};

/// Operator-facing / persisted interrupted-tool stub (ADR-0039 Q6/Q7).
pub const INTERRUPTED_TOOL_RESULT: &str =
    "Interrupted: tool call cancelled by run timeout (not re-executed).";

/// Transcript snapshot for restart-safe continue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptCheckpoint {
    pub messages: Vec<ChatMessage>,
    pub iteration: u32,
    pub dangling_tool_ids: Vec<String>,
}

impl TranscriptCheckpoint {
    /// Build from in-flight loop messages; compute dangling tool ids.
    pub fn from_messages(messages: Vec<ChatMessage>, iteration: u32) -> Self {
        let dangling_tool_ids = find_dangling_tool_ids(&messages);
        Self {
            messages,
            iteration,
            dangling_tool_ids,
        }
    }

    /// Messages ready for the next model turn: dangling ids get a safe
    /// interrupted tool-result so the list stays well-formed.
    pub fn messages_for_continue(&self) -> Vec<ChatMessage> {
        let mut out = self.messages.clone();
        synthesize_interrupted_results(&mut out, &self.dangling_tool_ids);
        out
    }
}

/// Tool-call ids that have an assistant `tool_calls` entry but no tool result.
pub fn find_dangling_tool_ids(messages: &[ChatMessage]) -> Vec<String> {
    let answered: HashSet<&str> = messages
        .iter()
        .filter(|m| m.role == "tool")
        .filter_map(|m| m.tool_call_id.as_deref())
        .collect();
    let mut dangling = Vec::new();
    let mut seen = HashSet::new();
    for msg in messages {
        if let Some(calls) = &msg.tool_calls {
            for tc in calls {
                if !answered.contains(tc.id.as_str()) && seen.insert(tc.id.clone()) {
                    dangling.push(tc.id.clone());
                }
            }
        }
    }
    dangling
}

/// Append interrupted stubs for dangling ids that still lack a tool result.
pub fn synthesize_interrupted_results(messages: &mut Vec<ChatMessage>, dangling: &[String]) {
    let answered: HashSet<String> = messages
        .iter()
        .filter(|m| m.role == "tool")
        .filter_map(|m| m.tool_call_id.clone())
        .collect();
    for id in dangling {
        if answered.contains(id) {
            continue;
        }
        messages.push(ChatMessage {
            role: "tool".to_string(),
            content: Some(MessageContent::Text(INTERRUPTED_TOOL_RESULT.to_string())),
            tool_calls: None,
            tool_call_id: Some(id.clone()),
        });
    }
}

/// SQLite-backed store keyed by pending_reruns / queue id.
#[derive(Clone)]
pub struct CheckpointStore {
    conn: Arc<Mutex<Connection>>,
}

impl CheckpointStore {
    pub fn new(conn: Arc<Mutex<Connection>>) -> Self {
        Self { conn }
    }

    /// Idempotent schema for production boot and in-memory tests.
    pub fn ensure_schema(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS scheduler_transcript_checkpoints (
                queue_id TEXT PRIMARY KEY,
                run_id TEXT,
                messages_json TEXT NOT NULL,
                iteration INTEGER NOT NULL,
                dangling_json TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );",
        )
        .context("create scheduler_transcript_checkpoints")?;
        Ok(())
    }

    pub async fn ensure_schema_async(&self) -> Result<()> {
        let conn = self.conn.lock().await;
        Self::ensure_schema(&conn)
    }

    /// Upsert checkpoint for a human-gate queue row (timeout path only).
    pub async fn save(
        &self,
        queue_id: &str,
        run_id: Option<&str>,
        checkpoint: &TranscriptCheckpoint,
    ) -> Result<()> {
        let messages_json =
            serde_json::to_string(&checkpoint.messages).context("serialize checkpoint messages")?;
        let dangling_json = serde_json::to_string(&checkpoint.dangling_tool_ids)
            .context("serialize dangling tool ids")?;
        let conn = self.conn.lock().await;
        Self::ensure_schema(&conn)?;
        conn.execute(
            "INSERT INTO scheduler_transcript_checkpoints
             (queue_id, run_id, messages_json, iteration, dangling_json)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(queue_id) DO UPDATE SET
               run_id = excluded.run_id,
               messages_json = excluded.messages_json,
               iteration = excluded.iteration,
               dangling_json = excluded.dangling_json,
               created_at = datetime('now')",
            rusqlite::params![
                queue_id,
                run_id,
                messages_json,
                checkpoint.iteration as i64,
                dangling_json
            ],
        )
        .context("upsert transcript checkpoint")?;
        Ok(())
    }

    pub async fn get(&self, queue_id: &str) -> Result<Option<TranscriptCheckpoint>> {
        let conn = self.conn.lock().await;
        Self::ensure_schema(&conn)?;
        let mut stmt = conn.prepare(
            "SELECT messages_json, iteration, dangling_json
             FROM scheduler_transcript_checkpoints WHERE queue_id = ?1",
        )?;
        let mut rows = stmt.query(rusqlite::params![queue_id])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let messages_json: String = row.get(0)?;
        let iteration: i64 = row.get(1)?;
        let dangling_json: String = row.get(2)?;
        let messages: Vec<ChatMessage> =
            serde_json::from_str(&messages_json).context("deserialize checkpoint messages")?;
        let dangling_tool_ids: Vec<String> =
            serde_json::from_str(&dangling_json).context("deserialize dangling ids")?;
        Ok(Some(TranscriptCheckpoint {
            messages,
            iteration: iteration as u32,
            dangling_tool_ids,
        }))
    }

    /// Load and delete — used when Retry dispatches continue (consume).
    pub async fn take(&self, queue_id: &str) -> Result<Option<TranscriptCheckpoint>> {
        let cp = self.get(queue_id).await?;
        if cp.is_some() {
            self.clear(queue_id).await?;
        }
        Ok(cp)
    }

    pub async fn clear(&self, queue_id: &str) -> Result<bool> {
        let conn = self.conn.lock().await;
        Self::ensure_schema(&conn)?;
        let n = conn
            .execute(
                "DELETE FROM scheduler_transcript_checkpoints WHERE queue_id = ?1",
                rusqlite::params![queue_id],
            )
            .context("delete transcript checkpoint")?;
        Ok(n > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{FunctionCall, ToolCall};

    fn tool_call(id: &str) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            call_type: "function".to_string(),
            function: FunctionCall {
                name: "x".to_string(),
                arguments: "{}".to_string(),
            },
        }
    }

    #[test]
    fn find_dangling_and_synthesize() {
        let msgs = vec![
            ChatMessage {
                role: "assistant".into(),
                content: Some(MessageContent::Text("hi".into())),
                tool_calls: Some(vec![tool_call("a"), tool_call("b")]),
                tool_call_id: None,
            },
            ChatMessage {
                role: "tool".into(),
                content: Some(MessageContent::Text("ok".into())),
                tool_calls: None,
                tool_call_id: Some("a".into()),
            },
        ];
        let dangling = find_dangling_tool_ids(&msgs);
        assert_eq!(dangling, vec!["b".to_string()]);
        let cp = TranscriptCheckpoint::from_messages(msgs, 3);
        assert_eq!(cp.iteration, 3);
        assert_eq!(cp.dangling_tool_ids, vec!["b".to_string()]);
        let cont = cp.messages_for_continue();
        assert_eq!(cont.len(), 3);
        assert_eq!(cont[2].tool_call_id.as_deref(), Some("b"));
        assert!(cont[2]
            .content
            .as_ref()
            .unwrap()
            .as_text()
            .contains("not re-executed"));
        assert!(find_dangling_tool_ids(&cont).is_empty());
    }

    #[tokio::test]
    async fn save_get_take_clear() {
        let conn = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
        let store = CheckpointStore::new(conn);
        store.ensure_schema_async().await.unwrap();
        let cp = TranscriptCheckpoint {
            messages: vec![ChatMessage {
                role: "user".into(),
                content: Some(MessageContent::Text("p".into())),
                tool_calls: None,
                tool_call_id: None,
            }],
            iteration: 2,
            dangling_tool_ids: vec!["d1".into()],
        };
        store.save("qid-1", Some("run-1"), &cp).await.unwrap();
        let loaded = store.get("qid-1").await.unwrap().unwrap();
        assert_eq!(loaded.iteration, cp.iteration);
        assert_eq!(loaded.dangling_tool_ids, cp.dangling_tool_ids);
        assert_eq!(loaded.messages.len(), 1);
        let taken = store.take("qid-1").await.unwrap().unwrap();
        assert_eq!(taken.iteration, cp.iteration);
        assert_eq!(taken.dangling_tool_ids, cp.dangling_tool_ids);
        assert!(store.get("qid-1").await.unwrap().is_none());

        store.save("qid-2", None, &cp).await.unwrap();
        assert!(store.clear("qid-2").await.unwrap());
        assert!(store.get("qid-2").await.unwrap().is_none());
        assert!(!store.clear("qid-2").await.unwrap());
    }
}
