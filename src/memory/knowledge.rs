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

/// One archived knowledge change (trigger-written).
#[derive(Debug, Clone)]
pub struct KnowledgeVersion {
    pub id: String,
    pub category: String,
    pub key: String,
    pub old_value: Option<String>,
    pub new_value: Option<String>,
    pub source: Option<String>,
    pub change_type: String,
    pub changed_at: String,
}

/// Time-bounded triple.
#[derive(Debug, Clone)]
pub struct Fact {
    pub id: String,
    pub entity: String,
    pub relation: String,
    pub value: String,
    pub valid_from: String,
    pub valid_to: Option<String>,
    pub source: Option<String>,
    pub confidence: f64,
}

/// Input for [`MemoryStore::add_fact`].
#[derive(Debug, Clone)]
pub struct FactToAdd {
    pub entity: String,
    pub relation: String,
    pub value: String,
    pub valid_from: String,
    pub source: Option<String>,
    pub confidence: Option<f64>,
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
                    WHERE embedding MATCH ?1
                    ORDER BY distance
                    LIMIT ?2
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

    /// Full version timeline for a (category, key) pair, oldest first.
    pub async fn knowledge_timeline(
        &self,
        category: &str,
        key: &str,
    ) -> Result<Vec<KnowledgeVersion>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, category, key, old_value, new_value, source, change_type, changed_at
             FROM knowledge_history
             WHERE category = ?1 AND key = ?2
             ORDER BY changed_at ASC, rowid ASC",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![category, key], parse_knowledge_version)?
            .collect::<Result<Vec<_>, _>>()
            .context("Failed to load knowledge timeline")?;
        Ok(rows)
    }

    /// Reconstruct knowledge value as of a SQLite datetime string.
    pub async fn knowledge_as_of(
        &self,
        category: &str,
        key: &str,
        as_of: &str,
    ) -> Result<Option<String>> {
        let conn = self.conn.lock().await;

        let current: Option<(String, String)> = conn
            .query_row(
                "SELECT value, created_at FROM knowledge WHERE category = ?1 AND key = ?2",
                rusqlite::params![category, key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .ok();

        let mut stmt = conn.prepare(
            "SELECT id, category, key, old_value, new_value, source, change_type, changed_at
             FROM knowledge_history
             WHERE category = ?1 AND key = ?2
             ORDER BY changed_at DESC, rowid DESC",
        )?;
        let history: Vec<KnowledgeVersion> = stmt
            .query_map(rusqlite::params![category, key], parse_knowledge_version)?
            .collect::<Result<Vec<_>, _>>()
            .context("Failed to load knowledge history for as_of")?;

        // Reverse-apply changes newer than as_of onto the live value.
        let mut val = current.as_ref().map(|(v, _)| v.clone());
        for h in &history {
            if h.changed_at.as_str() <= as_of {
                break;
            }
            val = h.old_value.clone();
        }

        // Before first create and no residual history → absent.
        if let Some((_, ref created_at)) = current {
            if as_of < created_at.as_str() && history.is_empty() {
                return Ok(None);
            }
            if as_of < created_at.as_str() {
                let earliest = history.iter().map(|h| h.changed_at.as_str()).min();
                if earliest.is_none_or(|e| as_of < e) {
                    return Ok(None);
                }
            }
        }

        Ok(val)
    }

    /// Insert fact; skip if identical active exists. Auto-closes prior active for pair.
    pub async fn add_fact(&self, fact: FactToAdd) -> Result<String> {
        let conn = self.conn.lock().await;
        let confidence = fact.confidence.unwrap_or(1.0);

        let existing: Option<String> = conn
            .query_row(
                "SELECT id FROM facts
                 WHERE entity = ?1 AND relation = ?2 AND value = ?3 AND valid_to IS NULL",
                rusqlite::params![&fact.entity, &fact.relation, &fact.value],
                |row| row.get(0),
            )
            .ok();
        if let Some(id) = existing {
            return Ok(id);
        }

        conn.execute(
            "UPDATE facts SET valid_to = ?1
             WHERE entity = ?2 AND relation = ?3 AND valid_to IS NULL",
            rusqlite::params![&fact.valid_from, &fact.entity, &fact.relation],
        )
        .context("Failed to close prior active fact")?;

        let id = Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO facts (id, entity, relation, value, valid_from, source, confidence)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                &id,
                &fact.entity,
                &fact.relation,
                &fact.value,
                &fact.valid_from,
                &fact.source,
                confidence
            ],
        )
        .context("Failed to insert fact")?;
        Ok(id)
    }

    /// End currently-active fact for (entity, relation).
    pub async fn close_fact(&self, entity: &str, relation: &str, valid_to: &str) -> Result<bool> {
        let conn = self.conn.lock().await;
        let rows = conn
            .execute(
                "UPDATE facts SET valid_to = ?1
                 WHERE entity = ?2 AND relation = ?3 AND valid_to IS NULL",
                rusqlite::params![valid_to, entity, relation],
            )
            .context("Failed to close fact")?;
        Ok(rows > 0)
    }

    /// Facts for entity; `as_of = None` → currently active only.
    pub async fn query_facts(&self, entity: &str, as_of: Option<&str>) -> Result<Vec<Fact>> {
        let conn = self.conn.lock().await;
        if let Some(as_of) = as_of {
            let mut stmt = conn.prepare(
                "SELECT id, entity, relation, value, valid_from, valid_to, source, confidence
                 FROM facts
                 WHERE entity = ?1
                   AND valid_from <= ?2
                   AND (valid_to IS NULL OR valid_to > ?2)
                 ORDER BY relation, valid_from",
            )?;
            let facts = stmt
                .query_map(rusqlite::params![entity, as_of], parse_fact)?
                .collect::<Result<Vec<_>, _>>()
                .context("Failed to query facts as_of")?;
            Ok(facts)
        } else {
            let mut stmt = conn.prepare(
                "SELECT id, entity, relation, value, valid_from, valid_to, source, confidence
                 FROM facts
                 WHERE entity = ?1 AND valid_to IS NULL
                 ORDER BY relation, valid_from",
            )?;
            let facts = stmt
                .query_map(rusqlite::params![entity], parse_fact)?
                .collect::<Result<Vec<_>, _>>()
                .context("Failed to query active facts")?;
            Ok(facts)
        }
    }

    /// Full timeline for one relation, oldest first.
    pub async fn fact_timeline(&self, entity: &str, relation: &str) -> Result<Vec<Fact>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, entity, relation, value, valid_from, valid_to, source, confidence
             FROM facts
             WHERE entity = ?1 AND relation = ?2
             ORDER BY valid_from ASC, created_at ASC",
        )?;
        let facts = stmt
            .query_map(rusqlite::params![entity, relation], parse_fact)?
            .collect::<Result<Vec<_>, _>>()
            .context("Failed to load fact timeline")?;
        Ok(facts)
    }

    /// Substring search across fact values (ponytail: LIKE, not FTS).
    pub async fn search_facts(&self, query: &str, limit: usize) -> Result<Vec<Fact>> {
        let conn = self.conn.lock().await;
        let pattern = format!("%{query}%");
        let mut stmt = conn.prepare(
            "SELECT id, entity, relation, value, valid_from, valid_to, source, confidence
             FROM facts
             WHERE value LIKE ?1 OR entity LIKE ?1 OR relation LIKE ?1
             ORDER BY created_at DESC
             LIMIT ?2",
        )?;
        let facts = stmt
            .query_map(rusqlite::params![pattern, limit as i64], parse_fact)?
            .collect::<Result<Vec<_>, _>>()
            .context("Failed to search facts")?;
        Ok(facts)
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

fn parse_knowledge_version(row: &rusqlite::Row) -> rusqlite::Result<KnowledgeVersion> {
    Ok(KnowledgeVersion {
        id: row.get(0)?,
        category: row.get(1)?,
        key: row.get(2)?,
        old_value: row.get(3)?,
        new_value: row.get(4)?,
        source: row.get(5)?,
        change_type: row.get(6)?,
        changed_at: row.get(7)?,
    })
}

fn parse_fact(row: &rusqlite::Row) -> rusqlite::Result<Fact> {
    Ok(Fact {
        id: row.get(0)?,
        entity: row.get(1)?,
        relation: row.get(2)?,
        value: row.get(3)?,
        valid_from: row.get(4)?,
        valid_to: row.get(5)?,
        source: row.get(6)?,
        confidence: row.get(7)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::MemoryStore;

    #[tokio::test]
    async fn test_knowledge_history_archives_on_update() {
        let store = MemoryStore::open_in_memory().unwrap();
        store.remember("pref", "brand", "Nike", None).await.unwrap();
        store
            .remember("pref", "brand", "Adidas", None)
            .await
            .unwrap();
        let tl = store.knowledge_timeline("pref", "brand").await.unwrap();
        assert_eq!(tl.len(), 1);
        assert_eq!(tl[0].old_value.as_deref(), Some("Nike"));
        assert_eq!(tl[0].new_value.as_deref(), Some("Adidas"));
        assert_eq!(tl[0].change_type, "update");
    }

    #[tokio::test]
    async fn test_knowledge_history_archives_on_delete() {
        let store = MemoryStore::open_in_memory().unwrap();
        store.remember("pref", "x", "1", None).await.unwrap();
        assert!(store.forget("pref", "x").await.unwrap());
        let tl = store.knowledge_timeline("pref", "x").await.unwrap();
        assert_eq!(tl.len(), 1);
        assert_eq!(tl[0].change_type, "delete");
        assert_eq!(tl[0].old_value.as_deref(), Some("1"));
        assert!(tl[0].new_value.is_none());
    }

    #[tokio::test]
    async fn test_knowledge_as_of_point_in_time() {
        let store = MemoryStore::open_in_memory().unwrap();
        store.remember("pref", "brand", "Nike", None).await.unwrap();
        // before any write
        assert!(store
            .knowledge_as_of("pref", "brand", "2000-01-01")
            .await
            .unwrap()
            .is_none());
        // after write (current)
        assert_eq!(
            store
                .knowledge_as_of("pref", "brand", "9999-01-01")
                .await
                .unwrap()
                .as_deref(),
            Some("Nike")
        );
        store
            .remember("pref", "brand", "Adidas", None)
            .await
            .unwrap();
        let tl = store.knowledge_timeline("pref", "brand").await.unwrap();
        let changed = &tl[0].changed_at;
        // just before the change → Nike (changed_at is second-resolution; use reverse path)
        // After full reverse of the Adidas update, old is Nike.
        // as_of equal to changed_at keeps the change (changed_at <= as_of means applied).
        assert_eq!(
            store
                .knowledge_as_of("pref", "brand", changed)
                .await
                .unwrap()
                .as_deref(),
            Some("Adidas")
        );
    }

    #[tokio::test]
    async fn test_add_and_query_fact() {
        let store = MemoryStore::open_in_memory().unwrap();
        let id = store
            .add_fact(FactToAdd {
                entity: "Kan".into(),
                relation: "prefers".into(),
                value: "Nike".into(),
                valid_from: "2024-09-01".into(),
                source: None,
                confidence: Some(0.9),
            })
            .await
            .unwrap();
        assert!(!id.is_empty());
        let facts = store.query_facts("Kan", None).await.unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].value, "Nike");
    }

    #[tokio::test]
    async fn test_idempotent_add_and_auto_close() {
        let store = MemoryStore::open_in_memory().unwrap();
        let a = store
            .add_fact(FactToAdd {
                entity: "Kan".into(),
                relation: "prefers".into(),
                value: "Nike".into(),
                valid_from: "2024-01-01".into(),
                source: None,
                confidence: None,
            })
            .await
            .unwrap();
        let a2 = store
            .add_fact(FactToAdd {
                entity: "Kan".into(),
                relation: "prefers".into(),
                value: "Nike".into(),
                valid_from: "2024-06-01".into(),
                source: None,
                confidence: None,
            })
            .await
            .unwrap();
        assert_eq!(a, a2);
        let _ = store
            .add_fact(FactToAdd {
                entity: "Kan".into(),
                relation: "prefers".into(),
                value: "Adidas".into(),
                valid_from: "2026-03-01".into(),
                source: None,
                confidence: None,
            })
            .await
            .unwrap();
        let active = store.query_facts("Kan", None).await.unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].value, "Adidas");
        let past = store.query_facts("Kan", Some("2025-06-01")).await.unwrap();
        assert_eq!(past.len(), 1);
        assert_eq!(past[0].value, "Nike");
        let tl = store.fact_timeline("Kan", "prefers").await.unwrap();
        assert_eq!(tl.len(), 2);
    }

    #[tokio::test]
    async fn test_close_fact() {
        let store = MemoryStore::open_in_memory().unwrap();
        store
            .add_fact(FactToAdd {
                entity: "Kan".into(),
                relation: "lives_in".into(),
                value: "HK".into(),
                valid_from: "2020-01-01".into(),
                source: None,
                confidence: None,
            })
            .await
            .unwrap();
        assert!(store
            .close_fact("Kan", "lives_in", "2026-01-01")
            .await
            .unwrap());
        assert!(store.query_facts("Kan", None).await.unwrap().is_empty());
        let past = store.query_facts("Kan", Some("2025-01-01")).await.unwrap();
        assert_eq!(past[0].value, "HK");
    }
}
