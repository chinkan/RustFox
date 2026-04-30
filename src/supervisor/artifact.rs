use anyhow::{Context, Result};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct ArtifactRow {
    pub id: String,
    pub kind: String,
    pub path: String,
}

pub struct ArtifactManager {
    root: PathBuf,
    conn: Arc<Mutex<Connection>>,
}

impl ArtifactManager {
    pub fn new(root: PathBuf, conn: Arc<Mutex<Connection>>) -> Self {
        Self { root, conn }
    }

    pub async fn write_text(
        &self,
        task_id: &str,
        job_id: Option<&str>,
        kind: &str,
        filename: &str,
        content: &str,
    ) -> Result<String> {
        let safe_content = crate::supervisor::redact::redact(content);
        let task_dir = self.root.join(task_id);
        tokio::fs::create_dir_all(&task_dir)
            .await
            .with_context(|| format!("create artifact dir {}", task_dir.display()))?;
        let path = task_dir.join(filename);
        tokio::fs::write(&path, &safe_content)
            .await
            .with_context(|| format!("write artifact {}", path.display()))?;

        let mut h = Sha256::new();
        h.update(safe_content.as_bytes());
        let sha = format!("{:x}", h.finalize());
        let bytes = safe_content.len() as i64;
        let id = Uuid::new_v4().to_string();
        let rel = path
            .strip_prefix(&self.root)
            .unwrap_or(&path)
            .to_string_lossy()
            .to_string();

        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO sup_artifacts (id, task_id, job_id, kind, path, sha256, bytes)
             VALUES (?1,?2,?3,?4,?5,?6,?7)",
            rusqlite::params![id, task_id, job_id, kind, rel, sha, bytes],
        )?;
        Ok(id)
    }

    pub async fn list(&self, task_id: &str) -> Result<Vec<ArtifactRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, kind, path FROM sup_artifacts WHERE task_id=?1 ORDER BY created_at ASC",
        )?;
        let rows = stmt
            .query_map([task_id], |r| {
                Ok(ArtifactRow {
                    id: r.get(0)?,
                    kind: r.get(1)?,
                    path: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn writes_artifact_and_indexes_in_db() {
        let dir = tempfile::tempdir().unwrap();
        let memory = crate::memory::MemoryStore::open_in_memory().unwrap();

        let store = crate::supervisor::store::TaskStore::new(memory.connection());
        let task = crate::supervisor::task::Task::new("T", "u");
        store.create(&task, "telegram", "u", None).await.unwrap();

        let am = ArtifactManager::new(dir.path().into(), memory.connection());
        let id = am
            .write_text(&task.id, None, "intake", "intake.json", r#"{"a":1}"#)
            .await
            .unwrap();

        assert!(dir.path().join(&task.id).join("intake.json").exists());
        let rows = am.list(&task.id).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, id);
        assert_eq!(rows[0].kind, "intake");
    }

    #[tokio::test]
    async fn write_text_redacts_secrets_before_persisting() {
        let dir = tempfile::tempdir().unwrap();
        let memory = crate::memory::MemoryStore::open_in_memory().unwrap();
        let store = crate::supervisor::store::TaskStore::new(memory.connection());
        let task = crate::supervisor::task::Task::new("T", "u");
        store.create(&task, "telegram", "u", None).await.unwrap();

        let am = ArtifactManager::new(dir.path().into(), memory.connection());
        am.write_text(
            &task.id,
            None,
            "log",
            "leak.txt",
            "creds: api_key=sk-supersecret-XYZ and Bearer leakytoken",
        )
        .await
        .unwrap();

        let on_disk = std::fs::read_to_string(dir.path().join(&task.id).join("leak.txt")).unwrap();
        assert!(
            !on_disk.contains("sk-supersecret-XYZ"),
            "secret leaked to disk: {on_disk}"
        );
        assert!(
            !on_disk.contains("leakytoken"),
            "secret leaked to disk: {on_disk}"
        );
        assert!(on_disk.contains("api_key=***"));
        assert!(on_disk.contains("Bearer ***"));
    }
}
