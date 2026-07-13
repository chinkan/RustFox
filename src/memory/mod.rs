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

use crate::config::MemoryConfig;
use crate::memory::embeddings::{EmbeddingConfig, EmbeddingEngine};

/// Thread-safe SQLite memory store with hybrid vector+FTS5 search
#[derive(Clone)]
pub struct MemoryStore {
    conn: Arc<Mutex<Connection>>,
    pub embeddings: Arc<EmbeddingEngine>,
    pub config: MemoryConfig,
}

impl MemoryStore {
    /// Open or create the SQLite database at the given path.
    /// If `embedding_config` is provided, vector search is enabled alongside FTS5.
    /// If None, falls back to FTS5-only search.
    pub fn open(
        path: &Path,
        embedding_config: Option<EmbeddingConfig>,
        memory_config: MemoryConfig,
    ) -> Result<Self> {
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
            config: memory_config,
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
            config: MemoryConfig::default(),
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

            -- Scheduled task execution history
            CREATE TABLE IF NOT EXISTS scheduled_task_runs (
                id          TEXT PRIMARY KEY,
                task_id     TEXT NOT NULL,
                run_at      TEXT NOT NULL,
                response    TEXT,
                error       TEXT,
                status      TEXT NOT NULL DEFAULT 'completed',
                created_at  TEXT NOT NULL DEFAULT (datetime('now')),
                FOREIGN KEY (task_id) REFERENCES scheduled_tasks(id)
            );

            CREATE INDEX IF NOT EXISTS idx_scheduled_task_runs_task
                ON scheduled_task_runs(task_id, run_at);

            -- Supervisor: tasks
            CREATE TABLE IF NOT EXISTS sup_tasks (
                id              TEXT PRIMARY KEY,
                title           TEXT NOT NULL,
                user_request    TEXT NOT NULL,
                task_type       TEXT NOT NULL,
                priority        INTEGER NOT NULL DEFAULT 5,
                risk_level      TEXT NOT NULL,
                execution_mode  TEXT NOT NULL,
                workflow        TEXT NOT NULL,
                state           TEXT NOT NULL,
                required_capabilities TEXT NOT NULL DEFAULT '[]',
                inputs          TEXT,
                constraints     TEXT,
                expected_outputs TEXT,
                approval_policy TEXT,
                platform        TEXT NOT NULL,
                user_id         TEXT NOT NULL,
                chat_id         TEXT,
                created_at      TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at      TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE INDEX IF NOT EXISTS idx_sup_tasks_state ON sup_tasks(state, updated_at);
            CREATE INDEX IF NOT EXISTS idx_sup_tasks_user  ON sup_tasks(user_id, state);

            -- Supervisor: jobs
            CREATE TABLE IF NOT EXISTS sup_jobs (
                id              TEXT PRIMARY KEY,
                task_id         TEXT NOT NULL,
                parent_job_id   TEXT,
                job_type        TEXT NOT NULL,
                backend         TEXT NOT NULL,
                goal            TEXT NOT NULL,
                prompt          TEXT,
                input_context   TEXT,
                timeout_secs    INTEGER NOT NULL,
                retry_max       INTEGER NOT NULL DEFAULT 0,
                retry_count     INTEGER NOT NULL DEFAULT 0,
                allow_tools     TEXT,
                workspace       TEXT,
                status          TEXT NOT NULL,
                result_summary  TEXT,
                result_evidence TEXT,
                error           TEXT,
                started_at      TEXT,
                finished_at     TEXT,
                FOREIGN KEY (task_id) REFERENCES sup_tasks(id)
            );
            CREATE INDEX IF NOT EXISTS idx_sup_jobs_task ON sup_jobs(task_id, status);

            -- Supervisor: state transitions
            CREATE TABLE IF NOT EXISTS sup_transitions (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                task_id     TEXT NOT NULL,
                from_state  TEXT NOT NULL,
                to_state    TEXT NOT NULL,
                reason      TEXT,
                actor       TEXT NOT NULL,
                occurred_at TEXT NOT NULL DEFAULT (datetime('now')),
                FOREIGN KEY (task_id) REFERENCES sup_tasks(id)
            );

            -- Supervisor: artifacts
            CREATE TABLE IF NOT EXISTS sup_artifacts (
                id          TEXT PRIMARY KEY,
                task_id     TEXT NOT NULL,
                job_id      TEXT,
                kind        TEXT NOT NULL,
                path        TEXT NOT NULL,
                sha256      TEXT,
                bytes       INTEGER,
                created_at  TEXT NOT NULL DEFAULT (datetime('now')),
                FOREIGN KEY (task_id) REFERENCES sup_tasks(id)
            );
            CREATE INDEX IF NOT EXISTS idx_sup_artifacts_task ON sup_artifacts(task_id, kind);
            ",
        )?;

        // Migration: add is_summarized column (safe no-op if column already exists)
        conn.execute_batch("ALTER TABLE messages ADD COLUMN is_summarized BOOLEAN DEFAULT 0;")
            .ok(); // ok() because ALTER TABLE fails if column already exists — that's intentional

        conn.execute_batch("ALTER TABLE conversations ADD COLUMN is_archived INTEGER DEFAULT 0;")
            .ok(); // safe no-op: ALTER TABLE fails with "duplicate column" on re-run

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

        // Migration: ensure message_embeddings has metadata columns (is_summarized, role)
        // for pre-filtering, and no existing rows with NULL metadata.
        // ALTER TABLE is not supported for vec0, so we must DROP and recreate.
        if table_exists(conn, "message_embeddings") {
            let cols: Vec<String> = conn
                .prepare("PRAGMA table_info(message_embeddings)")
                .and_then(|mut stmt| {
                    stmt.query_map([], |row| row.get(1))?
                        .collect::<Result<Vec<_>, _>>()
                })
                .unwrap_or_default();

            let has_meta = cols.contains(&"is_summarized".to_string());

            let needs_recreate = if has_meta {
                // Columns exist but old rows may have NULL metadata because the
                // original INSERT didn't write metadata columns. Check for NULLs.
                conn.query_row(
                    "SELECT COUNT(*) FROM message_embeddings WHERE is_summarized IS NULL",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .map(|count| count > 0)
                .unwrap_or(false)
            } else {
                true
            };

            if needs_recreate {
                conn.execute_batch("DROP TABLE message_embeddings;")?;
                conn.execute_batch(&format!(
                    "CREATE VIRTUAL TABLE message_embeddings USING vec0(\
                     embedding float[{}], is_summarized integer, role text);",
                    dims
                ))?;
                info!(
                    "Migrated message_embeddings with metadata columns (is_summarized, role){}",
                    if has_meta {
                        " and rebuilt existing data"
                    } else {
                        ""
                    }
                );
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
    fn sup_tables_exist_after_migration() {
        let memory = MemoryStore::open_in_memory().unwrap();
        let conn = memory.connection();
        let conn = conn.blocking_lock();
        for tbl in ["sup_tasks", "sup_jobs", "sup_transitions", "sup_artifacts"] {
            let exists: bool = conn
                .query_row(
                    "SELECT count(*)>0 FROM sqlite_master WHERE type='table' AND name=?1",
                    [tbl],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(exists, "table {tbl} missing");
        }
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
