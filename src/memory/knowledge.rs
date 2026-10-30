use anyhow::{Context, Result};
use uuid::Uuid;

use super::MemoryStore;
use crate::memory::conversations::{f32_slice_to_bytes, f32_vec_to_bytes};

/// A knowledge entry the agent has learned
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct KnowledgeEntry {
    pub id: String,
    pub category: String,
    pub key: String,
    pub value: String,
    pub source: Option<String>,
}

impl MemoryStore {
    /// Store or update a knowledge entry with vector embedding
    pub async fn remember(
        &self,
        category: &str,
        key: &str,
        value: &str,
        source: Option<&str>,
    ) -> Result<()> {
        let id = Uuid::new_v4().to_string();

        // Generate embedding before DB lock (async HTTP call)
        let embed_text = format!("{}: {}", key, value);
        let embedding = self.embeddings.try_embed_one(&embed_text).await;

        let conn = self.conn.lock().await;

        // Check if entry exists (for update case — need to remove old embedding)
        let old_rowid: Option<i64> = conn
            .query_row(
                "SELECT rowid FROM knowledge WHERE category = ?1 AND key = ?2",
                rusqlite::params![category, key],
                |row| row.get(0),
            )
            .ok();

        if let Some(old_rowid) = old_rowid {
            conn.execute(
                "DELETE FROM knowledge_embeddings WHERE rowid = ?1",
                rusqlite::params![old_rowid],
            )?;
        }

        conn.execute(
            "INSERT INTO knowledge (id, category, key, value, source)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(category, key) DO UPDATE SET
                value = excluded.value,
                source = excluded.source,
                updated_at = datetime('now')",
            rusqlite::params![&id, category, key, value, source],
        )
        .context("Failed to store knowledge")?;

        // Get the rowid for embedding
        let rowid: i64 = conn.query_row(
            "SELECT rowid FROM knowledge WHERE category = ?1 AND key = ?2",
            rusqlite::params![category, key],
            |row| row.get(0),
        )?;

        // Store embedding if available
        if let Some(ref emb) = embedding {
            let embedding_bytes = f32_slice_to_bytes(emb);
            conn.execute(
                "INSERT INTO knowledge_embeddings (rowid, embedding) VALUES (?1, ?2)",
                rusqlite::params![rowid, embedding_bytes],
            )?;
        }

        Ok(())
    }

    /// Recall a specific knowledge entry by exact key
    pub async fn recall(&self, category: &str, key: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().await;
        let result = conn
            .query_row(
                "SELECT value FROM knowledge WHERE category = ?1 AND key = ?2",
                rusqlite::params![category, key],
                |row| row.get(0),
            )
            .ok();

        Ok(result)
    }

    /// Hybrid search across knowledge using Reciprocal Rank Fusion (vector + FTS5).
    /// Falls back to FTS5-only if embeddings are not available.
    pub async fn search_knowledge(&self, query: &str, limit: usize) -> Result<Vec<KnowledgeEntry>> {
        // FTS5 raises `syntax error near ""` on a blank MATCH, which used to
        // abort the entire lookup with "Failed to FTS-search knowledge".
        // A blank query cannot match anything anyway - return early.
        let query = query.trim();
        if query.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let query_embedding = self.embeddings.try_embed_one(query).await;

        let conn = self.conn.lock().await;

        if let Some(ref qe) = query_embedding {
            // Hybrid search with Reciprocal Rank Fusion
            let query_bytes = f32_vec_to_bytes(qe);
            let sql = "
                WITH vec_matches AS (
                    SELECT rowid, distance,
                           row_number() OVER (ORDER BY distance) as rank_number
                    FROM knowledge_embeddings
                    WHERE embedding MATCH ?1 AND k = ?2
                ),
                fts_matches AS (
                    SELECT rowid,
                           row_number() OVER (ORDER BY rank) as rank_number
                    FROM knowledge_fts
                    WHERE knowledge_fts MATCH ?3
                    LIMIT ?2
                )
                SELECT k.id, k.category, k.key, k.value, k.source,
                       coalesce(1.0 / (60 + fts.rank_number), 0.0) * 0.5
                       + coalesce(1.0 / (60 + vec.rank_number), 0.0) * 0.5 as combined_rank
                FROM knowledge k
                LEFT JOIN vec_matches vec ON k.rowid = vec.rowid
                LEFT JOIN fts_matches fts ON k.rowid = fts.rowid
                WHERE vec.rowid IS NOT NULL OR fts.rowid IS NOT NULL
                ORDER BY combined_rank DESC
                LIMIT ?2
            ";

            let search_limit = (limit * 3) as i64;
            let mut stmt = conn.prepare(sql)?;
            let entries = stmt
                .query_map(rusqlite::params![query_bytes, search_limit, query], |row| {
                    parse_knowledge_row(row)
                })?
                .collect::<Result<Vec<_>, _>>()
                .context("Failed to hybrid-search knowledge")?;

            Ok(entries.into_iter().take(limit).collect())
        } else {
            // FTS5-only fallback
            let sql = "
                SELECT k.id, k.category, k.key, k.value, k.source
                FROM knowledge k
                JOIN knowledge_fts fts ON k.rowid = fts.rowid
                WHERE knowledge_fts MATCH ?1
                ORDER BY fts.rank
                LIMIT ?2
            ";
            let mut stmt = conn.prepare(sql)?;
            let entries = stmt
                .query_map(rusqlite::params![query, limit as i64], |row| {
                    parse_knowledge_row(row)
                })?
                .collect::<Result<Vec<_>, _>>()
                .context("Failed to FTS-search knowledge")?;

            Ok(entries)
        }
    }

    /// Most recently updated knowledge entries (no query — for browsing).
    pub async fn recent_knowledge(&self, limit: usize) -> Result<Vec<KnowledgeEntry>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, category, key, value, source
             FROM knowledge
             ORDER BY updated_at DESC
             LIMIT ?1",
        )?;
        let entries = stmt
            .query_map(rusqlite::params![limit as i64], parse_knowledge_row)?
            .collect::<Result<Vec<_>, _>>()
            .context("Failed to list recent knowledge")?;
        Ok(entries)
    }

    /// List all knowledge in a category
    #[allow(dead_code)]
    pub async fn list_knowledge(&self, category: &str) -> Result<Vec<KnowledgeEntry>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, category, key, value, source
             FROM knowledge
             WHERE category = ?1
             ORDER BY key",
        )?;

        let entries = stmt
            .query_map(rusqlite::params![category], parse_knowledge_row)?
            .collect::<Result<Vec<_>, _>>()
            .context("Failed to list knowledge")?;

        Ok(entries)
    }

    /// Forget a knowledge entry
    #[allow(dead_code)]
    pub async fn forget(&self, category: &str, key: &str) -> Result<bool> {
        let conn = self.conn.lock().await;

        let rowid: Option<i64> = conn
            .query_row(
                "SELECT rowid FROM knowledge WHERE category = ?1 AND key = ?2",
                rusqlite::params![category, key],
                |row| row.get(0),
            )
            .ok();

        if let Some(rowid) = rowid {
            conn.execute(
                "DELETE FROM knowledge_embeddings WHERE rowid = ?1",
                rusqlite::params![rowid],
            )?;
        }

        let rows = conn.execute(
            "DELETE FROM knowledge WHERE category = ?1 AND key = ?2",
            rusqlite::params![category, key],
        )?;
        Ok(rows > 0)
    }
}

fn parse_knowledge_row(row: &rusqlite::Row) -> rusqlite::Result<KnowledgeEntry> {
    Ok(KnowledgeEntry {
        id: row.get(0)?,
        category: row.get(1)?,
        key: row.get(2)?,
        value: row.get(3)?,
        source: row.get(4)?,
    })
}

#[cfg(test)]
mod vec0_regression {
    //! Regression guards for the two 2026-10-30 retrieval bugs.
    //!
    //! Both surfaced as `file_processor` warnings that made every uploaded PDF
    //! report "document indexed, but no relevant sections found":
    //!   - `A LIMIT or 'k = ?' constraint is required on vec0 knn queries`
    //!   - `Failed to FTS-search knowledge`
    use super::*;

    async fn seeded_store() -> MemoryStore {
        let memory = MemoryStore::open_in_memory().unwrap();
        for (k, v) in [
            ("statement-reconcile", "always reconcile against printed balance"),
            ("statement-ocr", "scans come back empty from pdftotext"),
            ("unrelated", "the weather in Hong Kong"),
        ] {
            memory
                .remember("fact", k, v, Some("test"))
                .await
                .expect("remember");
        }
        memory
    }

    /// Bug 2: the vec0 KNN was written as `ORDER BY distance LIMIT ?2`, which
    /// sqlite-vec refuses once a window function sits in the same CTE. The
    /// production SQL must be accepted as written.
    #[test]
    fn hybrid_search_sql_is_accepted_by_sqlite_vec() {
        let memory = MemoryStore::open_in_memory().unwrap();
        let conn = memory.connection();
        let conn = conn.blocking_lock();

        let sql = "
            WITH vec_matches AS (
                SELECT rowid, distance,
                       row_number() OVER (ORDER BY distance) as rank_number
                FROM knowledge_embeddings
                WHERE embedding MATCH ?1 AND k = ?2
            ),
            fts_matches AS (
                SELECT rowid,
                       row_number() OVER (ORDER BY rank) as rank_number
                FROM knowledge_fts
                WHERE knowledge_fts MATCH ?3
                LIMIT ?2
            )
            SELECT k.id FROM knowledge k
            LEFT JOIN vec_matches vec ON k.rowid = vec.rowid
            LEFT JOIN fts_matches fts ON k.rowid = fts.rowid
            WHERE vec.rowid IS NOT NULL OR fts.rowid IS NOT NULL
            LIMIT ?2
        ";
        let blob = vec![0u8; 1536 * 4];
        let mut stmt = conn.prepare(sql).expect("vec0 SQL must parse");
        // Consume the rows: `query()` is lazy, so an un-drained result set would
        // pass without sqlite-vec ever evaluating the kNN.
        let rows: Vec<i64> = stmt
            .query_map(rusqlite::params![blob, 5i64, "statement"], |r| r.get(0))
            .expect("vec0 hybrid query must execute without the LIMIT/k error")
            .collect::<rusqlite::Result<_>>()
            .expect("rows");
        let _ = rows;
    }

    /// Bug 1: a blank query used to reach `knowledge_fts MATCH ''`, which FTS5
    /// rejects with `syntax error near ""`. It must short-circuit instead.
    #[tokio::test]
    async fn blank_query_returns_empty_instead_of_failing() {
        let memory = seeded_store().await;
        for q in ["", "   ", "\n\t "] {
            let hits = memory.search_knowledge(q, 5).await;
            assert!(hits.is_ok(), "blank query {q:?} must not error: {hits:?}");
            assert!(
                hits.unwrap().is_empty(),
                "blank query {q:?} must match nothing"
            );
        }
    }

    #[tokio::test]
    async fn limit_zero_returns_empty_instead_of_failing() {
        let memory = seeded_store().await;
        let hits = memory.search_knowledge("statement", 0).await.unwrap();
        assert!(hits.is_empty());
    }

    /// The happy path must still work after the guard.
    #[tokio::test]
    async fn real_query_still_returns_hits() {
        let memory = seeded_store().await;
        let hits = memory.search_knowledge("statement", 5).await.unwrap();
        assert!(!hits.is_empty(), "FTS fallback should find `statement` rows");
    }
}
