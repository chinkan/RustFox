pub mod conversations;
pub mod embeddings;
pub mod knowledge;
pub mod query_rewriter;
pub mod rag;
pub mod summarizer;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::info;

use crate::memory::embeddings::{EmbeddingConfig, EmbeddingEngine};

/// Thread-safe SQLite memory store with hybrid vector+FTS5 search
#[derive(Clone)]
pub struct MemoryStore {
    conn: Arc<Mutex<Connection>>,
    pub embeddings: Arc<EmbeddingEngine>,
}

impl MemoryStore {
    /// Open or create the SQLite database at the given path.
    /// If `embedding_config` is provided, vector search is enabled alongside FTS5.
    /// If None, falls back to FTS5-only search.
    pub fn open(path: &Path, embedding_config: Option<EmbeddingConfig>) -> Result<Self> {
        // Register sqlite-vec extension before opening any connection
        unsafe {
            type VecInitFn = unsafe extern "C" fn(
                *mut rusqlite::ffi::sqlite3,
                *mut *mut i8,
                *const rusqlite::ffi::sqlite3_api_routines,
            ) -> i32;
            rusqlite::ffi::sqlite3_auto_extension(Some(
                std::mem::transmute::<*const (), VecInitFn>(
                    sqlite_vec::sqlite3_vec_init as *const (),
                ),
            ));
        }

        let conn = Connection::open(path)
            .with_context(|| format!("Failed to open database: {}", path.display()))?;

        // Enable WAL mode for better concurrent read performance
        // journal_mode PRAGMA always returns the resulting mode, so use query_row
        let _: String = conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;

        let embeddings = EmbeddingEngine::new(embedding_config);

        // Run migrations on the raw connection before wrapping in Mutex.
        // This avoids blocking_lock() panic when called from async context.
        Self::run_migrations(&conn, embeddings.dimensions())?;

        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
            embeddings: Arc::new(embeddings),
        };

        info!("Memory store initialized at: {}", path.display());
        Ok(store)
    }

    /// Open an in-memory database (for testing)
    #[allow(dead_code)]
    pub fn open_in_memory() -> Result<Self> {
        unsafe {
            type VecInitFn = unsafe extern "C" fn(
                *mut rusqlite::ffi::sqlite3,
                *mut *mut i8,
                *const rusqlite::ffi::sqlite3_api_routines,
            ) -> i32;
            rusqlite::ffi::sqlite3_auto_extension(Some(
                std::mem::transmute::<*const (), VecInitFn>(
                    sqlite_vec::sqlite3_vec_init as *const (),
                ),
            ));
        }

        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;

        let embeddings = EmbeddingEngine::new(None);

        Self::run_migrations(&conn, embeddings.dimensions())?;

        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
            embeddings: Arc::new(embeddings),
        };
        Ok(store)
    }

    /// Expose the underlying connection for modules that share the DB.
    #[allow(dead_code)]
    pub fn connection(&self) -> Arc<Mutex<Connection>> {
        Arc::clone(&self.conn)
    }

    fn run_migrations(conn: &Connection, dims: usize) -> Result<()> {
        conn.execute_batch(
            "
            -- Conversations table
            CREATE TABLE IF NOT EXISTS conversations (
                id TEXT PRIMARY KEY,
                platform TEXT NOT NULL,
                user_id TEXT NOT NULL,
                started_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            -- Messages table
            CREATE TABLE IF NOT EXISTS messages (
                id TEXT PRIMARY KEY,
                conversation_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT,
                tool_calls TEXT,
                tool_call_id TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                FOREIGN KEY (conversation_id) REFERENCES conversations(id)
            );

            CREATE INDEX IF NOT EXISTS idx_messages_conversation
                ON messages(conversation_id, created_at);

            CREATE INDEX IF NOT EXISTS idx_conversations_user
                ON conversations(platform, user_id, updated_at);

            -- Knowledge table
            CREATE TABLE IF NOT EXISTS knowledge (
                id TEXT PRIMARY KEY,
                category TEXT NOT NULL,
                key TEXT NOT NULL,
                value TEXT NOT NULL,
                source TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE UNIQUE INDEX IF NOT EXISTS idx_knowledge_key
                ON knowledge(category, key);

            -- FTS5 virtual tables for full-text search
            CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
                content,
                content=messages,
                content_rowid=rowid
            );

            CREATE VIRTUAL TABLE IF NOT EXISTS knowledge_fts USING fts5(
                key,
                value,
                content=knowledge,
                content_rowid=rowid
            );

            -- Triggers to keep FTS in sync
            CREATE TRIGGER IF NOT EXISTS messages_fts_insert AFTER INSERT ON messages
            WHEN NEW.content IS NOT NULL BEGIN
                INSERT INTO messages_fts(rowid, content) VALUES (NEW.rowid, NEW.content);
            END;

            CREATE TRIGGER IF NOT EXISTS messages_fts_delete AFTER DELETE ON messages
            WHEN OLD.content IS NOT NULL BEGIN
                INSERT INTO messages_fts(messages_fts, rowid, content)
                    VALUES('delete', OLD.rowid, OLD.content);
            END;

            CREATE TRIGGER IF NOT EXISTS knowledge_fts_insert AFTER INSERT ON knowledge BEGIN
                INSERT INTO knowledge_fts(rowid, key, value)
                    VALUES (NEW.rowid, NEW.key, NEW.value);
            END;

            CREATE TRIGGER IF NOT EXISTS knowledge_fts_delete AFTER DELETE ON knowledge BEGIN
                INSERT INTO knowledge_fts(knowledge_fts, rowid, key, value)
                    VALUES('delete', OLD.rowid, OLD.key, OLD.value);
            END;

            CREATE TRIGGER IF NOT EXISTS knowledge_fts_update AFTER UPDATE ON knowledge BEGIN
                INSERT INTO knowledge_fts(knowledge_fts, rowid, key, value)
                    VALUES('delete', OLD.rowid, OLD.key, OLD.value);
                INSERT INTO knowledge_fts(rowid, key, value)
                    VALUES (NEW.rowid, NEW.key, NEW.value);
            END;

            -- Schema metadata (e.g. embedding dimension for vec tables)
            CREATE TABLE IF NOT EXISTS schema_meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            -- Scheduled tasks for user-registered reminders / recurring jobs
            CREATE TABLE IF NOT EXISTS scheduled_tasks (
                id               TEXT PRIMARY KEY,
                scheduler_job_id TEXT,
                user_id          TEXT NOT NULL,
                chat_id          TEXT NOT NULL,
                platform         TEXT NOT NULL,
                trigger_type     TEXT NOT NULL,
                trigger_value    TEXT NOT NULL,
                prompt           TEXT NOT NULL,
                description      TEXT NOT NULL,
                status           TEXT NOT NULL DEFAULT 'active',
                created_at       TEXT NOT NULL DEFAULT (datetime('now')),
                next_run_at      TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_scheduled_tasks_user
                ON scheduled_tasks(user_id, status);
            ",
        )?;

        // Migration: add is_summarized column (safe no-op if column already exists)
        conn.execute_batch("ALTER TABLE messages ADD COLUMN is_summarized BOOLEAN DEFAULT 0;")
            .ok(); // ok() because ALTER TABLE fails if column already exists — that's intentional

        // Stored embedding dimension (None if legacy DB without schema_meta row)
        let raw: Option<String> = conn
            .query_row(
                "SELECT value FROM schema_meta WHERE key = 'embedding_dims'",
                [],
                |row| row.get(0),
            )
            .optional()
            .context("schema_meta query")?;
        let stored_dims: Option<usize> = raw.and_then(|s| s.parse().ok());

        let need_migrate = !matches!(stored_dims, Some(s) if s == dims);

        let table_exists = |conn: &Connection, name: &str| -> bool {
            conn.query_row(
                &format!(
                    "SELECT count(*) > 0 FROM sqlite_master WHERE type='table' AND name='{}'",
                    name
                ),
                [],
                |row| row.get(0),
            )
            .unwrap_or(false)
        };

        if need_migrate {
            // Drop vec tables so we can recreate with new dimension
            if table_exists(conn, "message_embeddings") {
                conn.execute_batch("DROP TABLE message_embeddings;")?;
            }
            if table_exists(conn, "knowledge_embeddings") {
                conn.execute_batch("DROP TABLE knowledge_embeddings;")?;
            }
            conn.execute_batch(&format!(
                "CREATE VIRTUAL TABLE message_embeddings USING vec0(embedding float[{}]);",
                dims
            ))?;
            conn.execute_batch(&format!(
                "CREATE VIRTUAL TABLE knowledge_embeddings USING vec0(embedding float[{}]);",
                dims
            ))?;
            conn.execute(
                "INSERT OR REPLACE INTO schema_meta (key, value) VALUES ('embedding_dims', ?1)",
                [dims.to_string()],
            )?;
            if let Some(prev_dims) = stored_dims {
                info!(
                    "Embedding dimension changed from {} to {}; vector tables recreated.",
                    prev_dims, dims
                );
            }
        } else {
            // Create vec tables only if they don't exist (same dimension)
            if !table_exists(conn, "message_embeddings") {
                conn.execute_batch(&format!(
                    "CREATE VIRTUAL TABLE message_embeddings USING vec0(embedding float[{}]);",
                    dims
                ))?;
                conn.execute(
                    "INSERT OR REPLACE INTO schema_meta (key, value) VALUES ('embedding_dims', ?1)",
                    [dims.to_string()],
                )?;
            }
            if !table_exists(conn, "knowledge_embeddings") {
                conn.execute_batch(&format!(
                    "CREATE VIRTUAL TABLE knowledge_embeddings USING vec0(embedding float[{}]);",
                    dims
                ))?;
                if stored_dims.is_none() {
                    conn.execute(
                        "INSERT OR REPLACE INTO schema_meta (key, value) VALUES ('embedding_dims', ?1)",
                        [dims.to_string()],
                    )?;
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scheduled_tasks_table_exists() {
        let memory = MemoryStore::open_in_memory().unwrap();
        let conn = memory.connection();
        let conn = conn.blocking_lock();
        let exists: bool = conn
            .query_row(
                "SELECT count(*) > 0 FROM sqlite_master WHERE type='table' AND name='scheduled_tasks'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(exists);
    }

    #[test]
    fn test_connection_accessor_returns_working_connection() {
        let memory = MemoryStore::open_in_memory().unwrap();
        let conn = memory.connection();
        let conn = conn.blocking_lock();
        let n: i64 = conn.query_row("SELECT 42", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 42);
    }
}
