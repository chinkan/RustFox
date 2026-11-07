use anyhow::{Context, Result};
use rusqlite::OptionalExtension;
use uuid::Uuid;

use super::MemoryStore;
use crate::memory::conversations::{f32_slice_to_bytes, f32_vec_to_bytes};
use crate::memory::evidence::{evidence_column_json, StoredEvidence};

/// A knowledge entry the agent has learned
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct KnowledgeEntry {
    pub id: String,
    pub category: String,
    pub key: String,
    pub value: String,
    pub source: Option<String>,
    /// JSON object `{kind, ref, captured_at}` or an array of those.
    /// Missing or `unknown` is not a trusted source.
    pub evidence: Option<String>,
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
        self.remember_with_evidence(category, key, value, source, None)
            .await
    }

    /// Store or update a knowledge entry.
    ///
    /// `evidence == None` writes `{"kind":"unknown"}` (untrusted). A later call
    /// replaces evidence. `source == None` leaves an existing source in place,
    /// so the remember tool can omit source without erasing a filename.
    pub async fn remember_with_evidence(
        &self,
        category: &str,
        key: &str,
        value: &str,
        source: Option<&str>,
        evidence: Option<&StoredEvidence>,
    ) -> Result<()> {
        let evidence_json = evidence_column_json(evidence)?;
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
            "INSERT INTO knowledge (id, category, key, value, source, evidence)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(category, key) DO UPDATE SET
                value = excluded.value,
                source = COALESCE(excluded.source, knowledge.source),
                evidence = excluded.evidence,
                updated_at = datetime('now')",
            rusqlite::params![&id, category, key, value, source, evidence_json],
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

    /// Recall the full row, including provenance. Settings lookups keep using
    /// [`Self::recall`], which returns the raw value only.
    pub async fn recall_entry(&self, category: &str, key: &str) -> Result<Option<KnowledgeEntry>> {
        let conn = self.conn.lock().await;
        let result = conn
            .query_row(
                "SELECT id, category, key, value, source, evidence
                 FROM knowledge WHERE category = ?1 AND key = ?2",
                rusqlite::params![category, key],
                parse_knowledge_row,
            )
            .optional()
            .context("Failed to recall knowledge")?;
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
                SELECT k.id, k.category, k.key, k.value, k.source, k.evidence,
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
                SELECT k.id, k.category, k.key, k.value, k.source, k.evidence
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
            "SELECT id, category, key, value, source, evidence
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
            "SELECT id, category, key, value, source, evidence
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
        evidence: row.get(5)?,
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
            (
                "statement-reconcile",
                "always reconcile against printed balance",
            ),
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
        assert!(
            !hits.is_empty(),
            "FTS fallback should find `statement` rows"
        );
    }
}

#[cfg(test)]
mod evidence_provenance {
    use super::*;
    use crate::memory::evidence::{
        evidence_column_json, format_evidence_suffix, EvidenceKind, EvidencePointer,
        StoredEvidence, UNTRUSTED_EVIDENCE_MARK,
    };
    use rusqlite::Connection;

    fn run_pointer() -> StoredEvidence {
        StoredEvidence::One(EvidencePointer {
            kind: EvidenceKind::Run,
            reference: Some("run-42".to_string()),
            captured_at: Some("2026-11-03T00:00:00Z".to_string()),
        })
    }

    #[tokio::test]
    async fn remember_without_evidence_stores_unknown_and_recall_value_stays_raw() {
        let memory = MemoryStore::open_in_memory().unwrap();
        memory
            .remember("settings", "mode", "minimal", None)
            .await
            .unwrap();
        assert_eq!(
            memory.recall("settings", "mode").await.unwrap().as_deref(),
            Some("minimal")
        );
        let entry = memory
            .recall_entry("settings", "mode")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            format_evidence_suffix(entry.evidence.as_deref()),
            UNTRUSTED_EVIDENCE_MARK
        );
        assert_eq!(entry.evidence.as_deref(), Some(r#"{"kind":"unknown"}"#));
    }

    #[tokio::test]
    async fn remember_with_evidence_roundtrips_through_recall_and_search() {
        let memory = MemoryStore::open_in_memory().unwrap();
        let evidence = run_pointer();
        memory
            .remember_with_evidence(
                "lesson",
                "status-untrusted",
                "scheduled_task_runs.status is not proof",
                None,
                Some(&evidence),
            )
            .await
            .unwrap();
        let entry = memory
            .recall_entry("lesson", "status-untrusted")
            .await
            .unwrap()
            .unwrap();
        let parsed: StoredEvidence =
            serde_json::from_str(entry.evidence.as_deref().unwrap()).unwrap();
        assert_eq!(parsed, evidence);
        let hits = memory.search_knowledge("proof", 5).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].evidence, entry.evidence);
        let recent = memory.recent_knowledge(5).await.unwrap();
        assert_eq!(recent[0].evidence, entry.evidence);
    }

    #[tokio::test]
    async fn update_without_source_keeps_the_old_source_and_omitted_evidence_is_unknown() {
        let memory = MemoryStore::open_in_memory().unwrap();
        let file = StoredEvidence::One(EvidencePointer {
            kind: EvidenceKind::File,
            reference: Some("notes.pdf".to_string()),
            captured_at: None,
        });
        memory
            .remember_with_evidence("document_chunk", "k", "old", Some("notes.pdf"), Some(&file))
            .await
            .unwrap();
        memory
            .remember("document_chunk", "k", "new", None)
            .await
            .unwrap();
        let entry = memory
            .recall_entry("document_chunk", "k")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(entry.value, "new");
        assert_eq!(entry.source.as_deref(), Some("notes.pdf"));
        assert_eq!(entry.evidence.as_deref(), Some(r#"{"kind":"unknown"}"#));
    }

    #[tokio::test]
    async fn update_with_a_new_source_replaces_it() {
        let memory = MemoryStore::open_in_memory().unwrap();
        memory
            .remember("document_chunk", "k", "old", Some("a.pdf"))
            .await
            .unwrap();
        memory
            .remember("document_chunk", "k", "new", Some("b.pdf"))
            .await
            .unwrap();
        let entry = memory
            .recall_entry("document_chunk", "k")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(entry.source.as_deref(), Some("b.pdf"));
    }

    #[tokio::test]
    async fn evidence_array_is_stored_as_an_array() {
        let memory = MemoryStore::open_in_memory().unwrap();
        let evidence = StoredEvidence::Many(vec![
            EvidencePointer {
                kind: EvidenceKind::Dream,
                reference: Some("know-1".to_string()),
                captured_at: None,
            },
            EvidencePointer {
                kind: EvidenceKind::File,
                reference: Some("a.pdf".to_string()),
                captured_at: Some("2026-11-07T00:00:00Z".to_string()),
            },
        ]);
        memory
            .remember_with_evidence("fact", "merged", "derived", None, Some(&evidence))
            .await
            .unwrap();
        let entry = memory
            .recall_entry("fact", "merged")
            .await
            .unwrap()
            .unwrap();
        let parsed: StoredEvidence =
            serde_json::from_str(entry.evidence.unwrap().as_str()).unwrap();
        assert_eq!(parsed, evidence);
        assert!(evidence_column_json(Some(&evidence))
            .unwrap()
            .starts_with('['));
    }

    #[test]
    fn legacy_rows_backfill_unknown_or_file_and_a_later_open_does_not_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "
                CREATE TABLE knowledge (
                    id TEXT PRIMARY KEY,
                    category TEXT NOT NULL,
                    key TEXT NOT NULL,
                    value TEXT NOT NULL,
                    source TEXT,
                    created_at TEXT NOT NULL DEFAULT (datetime('now')),
                    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
                );
                INSERT INTO knowledge (id, category, key, value, source) VALUES
                    ('1', 'fact', 'plain', 'no source', NULL),
                    ('2', 'document_chunk', 'chunk', 'pdf text', 'file.pdf'),
                    ('3', 'fact', 'note', 'words', 'just words'),
                    ('4', 'fact', 'link', 'paper', 'https://arxiv.org/abs/2610.40350');
                ",
            )
            .unwrap();
        }

        let memory =
            MemoryStore::open(&path, None, crate::config::MemoryConfig::default()).unwrap();
        let conn = memory.connection();
        let conn = conn.blocking_lock();
        let evidence_of = |key: &str| -> String {
            conn.query_row(
                "SELECT evidence FROM knowledge WHERE key = ?1",
                [key],
                |row| row.get(0),
            )
            .unwrap()
        };
        assert_eq!(evidence_of("plain"), r#"{"kind":"unknown"}"#);
        assert_eq!(evidence_of("note"), r#"{"kind":"unknown"}"#);
        assert_eq!(evidence_of("link"), r#"{"kind":"unknown"}"#);
        let file: StoredEvidence = serde_json::from_str(&evidence_of("chunk")).unwrap();
        assert_eq!(
            file,
            StoredEvidence::One(EvidencePointer {
                kind: EvidenceKind::File,
                reference: Some("file.pdf".to_string()),
                captured_at: None,
            })
        );
        // Rows inserted before knowledge_fts existed are not in the index.
        // An UPDATE would then hit FTS5's delete command. Index them first;
        // the backfill itself only writes the evidence column.
        conn.execute_batch(
            "INSERT INTO knowledge_fts(rowid, key, value)
             SELECT rowid, key, value FROM knowledge;",
        )
        .unwrap();
        conn.execute(
            "UPDATE knowledge SET evidence = ?1 WHERE key = 'plain'",
            [r#"{"kind":"run","ref":"run-kept"}"#],
        )
        .unwrap();
        drop(conn);
        drop(memory);

        let memory =
            MemoryStore::open(&path, None, crate::config::MemoryConfig::default()).unwrap();
        let conn = memory.connection();
        let conn = conn.blocking_lock();
        let kept: String = conn
            .query_row(
                "SELECT evidence FROM knowledge WHERE key = 'plain'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(kept, r#"{"kind":"run","ref":"run-kept"}"#);
    }
}
