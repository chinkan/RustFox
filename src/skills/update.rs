use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

use super::seed::{copy_dir_recursive_pub, hash_skill_dir};

/// Home-side lock file: skill-name → content hash captured at seed/update time.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SkillLock {
    pub version: u32,
    #[serde(default)]
    pub skills: BTreeMap<String, String>,
}

/// Outcome of an update run.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct UpdateReport {
    pub updated: Vec<String>,
    pub backed_up: Vec<String>,
    pub skipped: Vec<String>,
}

impl UpdateReport {
    pub fn summary(&self) -> String {
        format!(
            "Update: {} updated, {} backed-up, {} unchanged.",
            self.updated.len(),
            self.backed_up.len(),
            self.skipped.len()
        )
    }
}

/// Read the lock file at `lock_path`, or an empty lock if absent/invalid.
fn read_lock(lock_path: &Path) -> SkillLock {
    std::fs::read_to_string(lock_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(SkillLock {
            version: 1,
            skills: BTreeMap::new(),
        })
}

fn write_lock(lock_path: &Path, lock: &SkillLock) -> Result<()> {
    let json = serde_json::to_string_pretty(lock)?;
    std::fs::write(lock_path, json)
        .with_context(|| format!("Failed to write lock {}", lock_path.display()))
}

/// Re-sync `bundled` → `instance` using content hashes recorded in `lock_path`.
///
/// For each bundled skill:
/// - missing in instance → copy in (updated)
/// - present, instance hash == bundled hash → unchanged (skipped)
/// - present, instance hash == lock hash (and != bundled) → overwrite (updated)
/// - present, instance hash differs from both → rename entire instance dir to
///   `<name>.bak` (preserving all files), then copy bundled dir in (backed_up)
///
/// Instance-only skills (absent from `bundled`) are never touched.
pub async fn update_skills(
    bundled: &Path,
    instance: &Path,
    lock_path: &Path,
) -> Result<UpdateReport> {
    let mut report = UpdateReport::default();
    if !bundled.is_dir() {
        anyhow::bail!("No bundled skills found at {}", bundled.display());
    }
    let mut lock = read_lock(lock_path);
    tokio::fs::create_dir_all(instance)
        .await
        .with_context(|| format!("Failed to create instance directory {}", instance.display()))?;

    let mut entries = tokio::fs::read_dir(bundled).await?;
    while let Some(entry) = entries.next_entry().await? {
        let src = entry.path();
        if !src.is_dir() {
            continue;
        }
        let Some(name) = src.file_name().and_then(|n| n.to_str()).map(str::to_string) else {
            continue;
        };
        let dst = instance.join(&name);
        let bundled_hash = match hash_skill_dir(&src) {
            Some(h) => h,
            None => continue,
        };

        if !dst.exists() {
            copy_dir_recursive_pub(&src, &dst).await?;
            lock.skills.insert(name.clone(), bundled_hash);
            tracing::info!("Added skill '{name}' from bundle");
            report.updated.push(name);
            continue;
        }

        let instance_hash = hash_skill_dir(&dst);
        if instance_hash.as_deref() == Some(bundled_hash.as_str()) {
            lock.skills.entry(name.clone()).or_insert(bundled_hash);
            report.skipped.push(name);
            continue;
        }

        let lock_hash = lock.skills.get(&name).cloned();
        let unmodified = lock_hash.is_some() && lock_hash.as_deref() == instance_hash.as_deref();

        if !unmodified {
            // Rename the entire instance directory to <name>.bak, preserving all
            // user-added or user-modified files (not just SKILL.md/AGENT.md).
            let dst_bak = instance.join(format!("{name}.bak"));
            let _ = tokio::fs::remove_dir_all(&dst_bak).await;
            tokio::fs::rename(&dst, &dst_bak)
                .await
                .with_context(|| format!("Failed to back up '{name}' to {}", dst_bak.display()))?;
            copy_dir_recursive_pub(&src, &dst).await?;
            lock.skills.insert(name.clone(), bundled_hash);
            tracing::info!("Updated locally-modified skill '{name}' (backup saved as {name}.bak)");
            report.backed_up.push(name);
        } else {
            let _ = tokio::fs::remove_dir_all(&dst).await;
            copy_dir_recursive_pub(&src, &dst).await?;
            lock.skills.insert(name.clone(), bundled_hash);
            tracing::info!("Updated skill '{name}' from bundle");
            report.updated.push(name);
        }
    }

    tracing::info!("{}", report.summary());
    write_lock(lock_path, &lock)?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::super::seed::lock_map_for;
    use super::*;

    fn write_skill(root: &Path, name: &str, body: &str) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), body).unwrap();
    }

    fn seed_lock(lock_path: &Path, instance: &Path) {
        let lock = SkillLock {
            version: 1,
            skills: lock_map_for(instance),
        };
        write_lock(lock_path, &lock).unwrap();
    }

    #[tokio::test]
    async fn unchanged_skill_is_overwritten_with_new_bundle() {
        let tmp = tempfile::tempdir().unwrap();
        let bundled = tmp.path().join("bundled");
        let instance = tmp.path().join("instance");
        let lock = tmp.path().join("skills-lock.json");
        // instance currently matches the OLD bundle
        write_skill(&instance, "alpha", "v1");
        seed_lock(&lock, &instance);
        // bundle now has v2
        write_skill(&bundled, "alpha", "v2");

        let report = update_skills(&bundled, &instance, &lock).await.unwrap();
        assert_eq!(report.updated, vec!["alpha".to_string()]);
        assert!(report.backed_up.is_empty());
        assert_eq!(
            std::fs::read_to_string(instance.join("alpha/SKILL.md")).unwrap(),
            "v2"
        );
    }

    #[tokio::test]
    async fn locally_modified_skill_is_backed_up() {
        let tmp = tempfile::tempdir().unwrap();
        let bundled = tmp.path().join("bundled");
        let instance = tmp.path().join("instance");
        let lock = tmp.path().join("skills-lock.json");
        write_skill(&instance, "alpha", "v1");
        seed_lock(&lock, &instance);
        // user edited the instance copy
        std::fs::write(instance.join("alpha/SKILL.md"), "user-edit").unwrap();
        // bundle moved to v2
        write_skill(&bundled, "alpha", "v2");

        let report = update_skills(&bundled, &instance, &lock).await.unwrap();
        assert_eq!(report.backed_up, vec!["alpha".to_string()]);
        assert_eq!(
            std::fs::read_to_string(instance.join("alpha/SKILL.md")).unwrap(),
            "v2"
        );
        assert_eq!(
            std::fs::read_to_string(instance.join("alpha.bak/SKILL.md")).unwrap(),
            "user-edit"
        );
    }

    #[tokio::test]
    async fn instance_only_skill_is_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let bundled = tmp.path().join("bundled");
        let instance = tmp.path().join("instance");
        let lock = tmp.path().join("skills-lock.json");
        write_skill(&bundled, "alpha", "v1");
        write_skill(&instance, "alpha", "v1");
        write_skill(&instance, "mine", "private");
        seed_lock(&lock, &instance);

        let report = update_skills(&bundled, &instance, &lock).await.unwrap();
        // alpha unchanged (same hash), mine never visited
        assert!(report.updated.is_empty());
        assert!(report.backed_up.is_empty());
        assert_eq!(
            std::fs::read_to_string(instance.join("mine/SKILL.md")).unwrap(),
            "private"
        );
    }

    #[tokio::test]
    async fn unchanged_skill_without_lock_entry_seeds_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let bundled = tmp.path().join("bundled");
        let instance = tmp.path().join("instance");
        let lock = tmp.path().join("skills-lock.json");
        write_skill(&bundled, "alpha", "v1");
        write_skill(&instance, "alpha", "v1");

        let report = update_skills(&bundled, &instance, &lock).await.unwrap();
        assert_eq!(report.skipped, vec!["alpha".to_string()]);

        let seeded = read_lock(&lock);
        assert!(
            seeded.skills.contains_key("alpha"),
            "skip path should seed missing lock entry"
        );
    }

    #[tokio::test]
    async fn unchanged_skill_preserves_existing_lock_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let bundled = tmp.path().join("bundled");
        let instance = tmp.path().join("instance");
        let lock = tmp.path().join("skills-lock.json");
        write_skill(&bundled, "alpha", "v1");
        write_skill(&instance, "alpha", "v1");

        write_lock(
            &lock,
            &SkillLock {
                version: 1,
                skills: BTreeMap::from([("alpha".to_string(), "old-hash".to_string())]),
            },
        )
        .unwrap();

        let report = update_skills(&bundled, &instance, &lock).await.unwrap();
        assert_eq!(report.skipped, vec!["alpha".to_string()]);

        let seeded = read_lock(&lock);
        assert_eq!(seeded.skills.get("alpha"), Some(&"old-hash".to_string()));
    }
}
