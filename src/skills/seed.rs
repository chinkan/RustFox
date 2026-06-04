use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// SHA-256 (hex) of a skill/agent directory tree.
/// Requires a primary markdown (`SKILL.md` or `AGENT.md`) to exist.
pub fn hash_skill_dir(dir: &Path) -> Option<String> {
    ["SKILL.md", "AGENT.md"]
        .into_iter()
        .map(|f| dir.join(f))
        .find(|p| p.is_file())?;

    let mut files: Vec<PathBuf> = Vec::new();
    collect_files(dir, &mut files).ok()?;
    files.sort();

    let mut h = Sha256::new();
    for file in files {
        let rel = file.strip_prefix(dir).ok()?;
        let rel = rel.to_string_lossy().replace('\\', "/");
        h.update(rel.as_bytes());
        h.update([0]);
        h.update(std::fs::read(&file).ok()?);
    }
    Some(format!("{:x}", h.finalize()))
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, out)?;
        } else if path.is_file() {
            out.push(path);
        }
    }
    Ok(())
}

/// Copy every skill subdirectory from `bundled` into `instance` when `instance`
/// is empty or missing. Returns the number of skills copied (0 if skipped).
pub async fn seed_dir_if_empty(bundled: &Path, instance: &Path) -> Result<usize> {
    // If the bundled source and instance resolve to the same place, nothing to do.
    if let (Ok(a), Ok(b)) = (bundled.canonicalize(), instance.canonicalize()) {
        if a == b {
            return Ok(0);
        }
    }
    if !bundled.is_dir() {
        tracing::info!("No bundled skills at {}; skipping seed", bundled.display());
        return Ok(0);
    }
    // Non-empty instance → never overwrite.
    if instance.is_dir() {
        let mut entries = tokio::fs::read_dir(instance).await?;
        if entries.next_entry().await?.is_some() {
            return Ok(0);
        }
    } else {
        tokio::fs::create_dir_all(instance)
            .await
            .with_context(|| format!("Failed to create {}", instance.display()))?;
    }

    let mut copied = 0usize;
    let mut entries = tokio::fs::read_dir(bundled).await?;
    while let Some(entry) = entries.next_entry().await? {
        let src = entry.path();
        if !src.is_dir() {
            continue;
        }
        let Some(name) = src.file_name() else {
            continue;
        };
        let dst = instance.join(name);
        if let Err(e) = copy_dir_recursive(&src, &dst).await {
            tracing::warn!("Failed to seed {}: {}", src.display(), e);
            continue;
        }
        copied += 1;
    }
    tracing::info!("Seeded {copied} skill(s) into {}", instance.display());
    Ok(copied)
}

/// Public re-export wrapper so the update engine can reuse recursive copy.
pub async fn copy_dir_recursive_pub(src: &Path, dst: &Path) -> Result<()> {
    copy_dir_recursive(src, dst).await
}

/// Recursively copy `src` directory to `dst`.
async fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    tokio::fs::create_dir_all(dst).await?;
    let mut entries = tokio::fs::read_dir(src).await?;
    while let Some(entry) = entries.next_entry().await? {
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            Box::pin(copy_dir_recursive(&from, &to)).await?;
        } else {
            tokio::fs::copy(&from, &to).await?;
        }
    }
    Ok(())
}

/// Map of skill-name → content hash for every skill dir under `dir`.
pub fn lock_map_for(dir: &Path) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return map;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            if let (Some(name), Some(hash)) =
                (p.file_name().and_then(|n| n.to_str()), hash_skill_dir(&p))
            {
                map.insert(name.to_string(), hash);
            }
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_skill(root: &Path, name: &str, body: &str) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), body).unwrap();
    }

    #[tokio::test]
    async fn seeds_into_empty_instance() {
        let tmp = tempfile::tempdir().unwrap();
        let bundled = tmp.path().join("bundled");
        let instance = tmp.path().join("instance");
        write_skill(&bundled, "alpha", "---\nname: alpha\n---\nhi");
        write_skill(&bundled, "beta", "---\nname: beta\n---\nyo");

        let n = seed_dir_if_empty(&bundled, &instance).await.unwrap();
        assert_eq!(n, 2);
        assert!(instance.join("alpha/SKILL.md").is_file());
        assert!(instance.join("beta/SKILL.md").is_file());
    }

    #[tokio::test]
    async fn skips_when_instance_nonempty() {
        let tmp = tempfile::tempdir().unwrap();
        let bundled = tmp.path().join("bundled");
        let instance = tmp.path().join("instance");
        write_skill(&bundled, "alpha", "a");
        write_skill(&instance, "existing", "keep me");

        let n = seed_dir_if_empty(&bundled, &instance).await.unwrap();
        assert_eq!(n, 0);
        assert!(!instance.join("alpha").exists());
        assert!(instance.join("existing/SKILL.md").is_file());
    }

    #[test]
    fn lock_map_hashes_each_skill() {
        let tmp = tempfile::tempdir().unwrap();
        write_skill(tmp.path(), "alpha", "content-a");
        write_skill(tmp.path(), "beta", "content-b");
        let map = lock_map_for(tmp.path());
        assert_eq!(map.len(), 2);
        assert_ne!(map["alpha"], map["beta"]);
    }

    #[test]
    fn hash_skill_dir_changes_when_auxiliary_file_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join("alpha");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "content-a").unwrap();
        std::fs::write(skill_dir.join("guide.md"), "v1").unwrap();
        let before = hash_skill_dir(&skill_dir).expect("hash should exist");

        std::fs::write(skill_dir.join("guide.md"), "v2").unwrap();
        let after = hash_skill_dir(&skill_dir).expect("hash should exist");

        assert_ne!(
            before, after,
            "auxiliary file edits must affect directory hash"
        );
    }
}
