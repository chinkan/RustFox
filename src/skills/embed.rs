use anyhow::{Context, Result};
use include_dir::{include_dir, Dir};
use std::path::Path;

static BUNDLED_SKILLS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/skills");
static BUNDLED_AGENTS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/agents");

/// Outcome of an overwrite operation.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct OverwriteReport {
    /// Number of files written (new or overwritten).
    pub written: usize,
    /// Number of existing files that were backed up (contents differed from embedded).
    pub backed_up: usize,
}

/// Seed skills from embedded data into `instance_dir`.
/// Only writes if `instance_dir` is empty or missing.
pub async fn seed_skills(instance: &Path) -> Result<usize> {
    seed_from_embedded(&BUNDLED_SKILLS, instance).await
}

/// Seed agents from embedded data into `instance_dir`.
pub async fn seed_agents(instance: &Path) -> Result<usize> {
    seed_from_embedded(&BUNDLED_AGENTS, instance).await
}

/// Overwrite skills from embedded data into `instance_dir`, replacing all files.
/// Existing files whose content differs from the embedded version are backed up with a `.bak` suffix.
pub async fn overwrite_skills(instance: &Path) -> Result<OverwriteReport> {
    tokio::fs::create_dir_all(instance)
        .await
        .with_context(|| format!("Failed to create {}", instance.display()))?;
    let mut report = OverwriteReport::default();
    write_dir_tree_with_backup(&BUNDLED_SKILLS, instance, &mut report).await?;
    Ok(report)
}

/// Overwrite agents from embedded data into `instance_dir`, replacing all files.
/// Existing files whose content differs from the embedded version are backed up with a `.bak` suffix.
pub async fn overwrite_agents(instance: &Path) -> Result<OverwriteReport> {
    tokio::fs::create_dir_all(instance)
        .await
        .with_context(|| format!("Failed to create {}", instance.display()))?;
    let mut report = OverwriteReport::default();
    write_dir_tree_with_backup(&BUNDLED_AGENTS, instance, &mut report).await?;
    Ok(report)
}

async fn seed_from_embedded(embedded: &Dir<'static>, instance: &Path) -> Result<usize> {
    if instance.is_dir() {
        let mut entries = tokio::fs::read_dir(instance).await?;
        if entries.next_entry().await?.is_some() {
            tracing::info!(
                "{} is non-empty; skipping embedded seed",
                instance.display()
            );
            return Ok(0);
        }
    } else {
        tokio::fs::create_dir_all(instance).await?;
    }

    let mut count = 0;
    write_dir_tree(embedded, instance, &mut count).await?;
    tracing::info!(
        "Seeded {} file(s) from embedded data into {}",
        count,
        instance.display()
    );
    Ok(count)
}

/// Write embedded `Dir` tree under `base`, counting written files in `count`.
/// Uses a Vec of futures to avoid sync recursion.
async fn write_dir_tree(dir: &Dir<'_>, base: &Path, count: &mut usize) -> Result<()> {
    let mut futures = Vec::new();
    collect_writes(dir, base, &mut futures);
    for f in futures {
        let (path, contents) = f?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("Failed to create {}", parent.display()))?;
        }
        tokio::fs::write(&path, &contents)
            .await
            .with_context(|| format!("Failed to write {}", path.display()))?;
        *count += 1;
    }
    Ok(())
}

/// Like `write_dir_tree` but backs up existing files that differ before overwriting.
/// The existing file is renamed to `<path>.bak` if its content differs from the embedded version.
async fn write_dir_tree_with_backup(
    dir: &Dir<'_>,
    base: &Path,
    report: &mut OverwriteReport,
) -> Result<()> {
    let mut futures = Vec::new();
    collect_writes(dir, base, &mut futures);
    for f in futures {
        let (path, contents) = f?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("Failed to create {}", parent.display()))?;
        }
        if path.exists() {
            let existing = tokio::fs::read(&path).await?;
            if existing != contents {
                let backup = path.with_extension("bak");
                tokio::fs::rename(&path, &backup).await.with_context(|| {
                    format!(
                        "Failed to backup {} to {}",
                        path.display(),
                        backup.display()
                    )
                })?;
                report.backed_up += 1;
            }
        }
        tokio::fs::write(&path, &contents)
            .await
            .with_context(|| format!("Failed to write {}", path.display()))?;
        report.written += 1;
    }
    Ok(())
}

/// Collect (path, contents) pairs from an embedded directory tree.
fn collect_writes(
    dir: &Dir<'_>,
    base: &Path,
    out: &mut Vec<Result<(std::path::PathBuf, Vec<u8>)>>,
) {
    for file in dir.files() {
        let path = base.join(file.path());
        let contents = file.contents().to_vec();
        out.push(Ok((path, contents)));
    }
    for sub in dir.dirs() {
        collect_writes(sub, base, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_seed_skills_writes_files() {
        let tmp = tempfile::tempdir().unwrap();
        let instance = tmp.path().join("skills");
        let n = seed_skills(&instance).await.unwrap();
        assert!(n > 0, "should seed at least one skill file");
        assert!(instance.join("code-interpreter").join("SKILL.md").exists());
    }

    #[tokio::test]
    async fn test_seed_skills_skips_nonempty() {
        let tmp = tempfile::tempdir().unwrap();
        let instance = tmp.path().join("skills");
        tokio::fs::create_dir_all(&instance).await.unwrap();
        tokio::fs::write(instance.join("custom.md"), b"custom")
            .await
            .unwrap();

        let n = seed_skills(&instance).await.unwrap();
        assert_eq!(n, 0, "should skip when non-empty");
    }

    #[tokio::test]
    async fn test_seed_agents_writes_files() {
        let tmp = tempfile::tempdir().unwrap();
        let instance = tmp.path().join("agents");
        let n = seed_agents(&instance).await.unwrap();
        assert!(n > 0, "should seed at least one agent file");
    }
}

/// True when `name` is a skill directory shipped inside the binary.
pub fn is_bundled_skill(name: &str) -> bool {
    BUNDLED_SKILLS.get_dir(name).is_some()
}

/// True when `name` is an agent directory shipped inside the binary.
pub fn is_bundled_agent(name: &str) -> bool {
    BUNDLED_AGENTS.get_dir(name).is_some()
}

#[cfg(test)]
mod bundled_membership_tests {
    use super::*;

    #[test]
    fn known_bundled_skill_is_detected() {
        // The repo's own skills dir ships bundled; grab one name from it.
        let any = BUNDLED_SKILLS
            .dirs()
            .next()
            .expect("repo embeds at least one skill");
        assert!(is_bundled_skill(any.path().to_str().unwrap()));
    }

    #[test]
    fn arbitrary_name_is_not_bundled() {
        assert!(!is_bundled_skill("definitely-not-a-bundled-skill-9f3a"));
        assert!(!is_bundled_agent("nope-agent-1234"));
    }
}

/// Names of all top-level bundled skill directories (embedded at compile
/// time). Used as the authoritative "bundled" set for portal provenance.
pub fn bundled_skill_names() -> Vec<String> {
    BUNDLED_SKILLS
        .dirs()
        .filter_map(|d| d.path().to_str().map(str::to_string))
        .collect()
}

/// Names of all top-level bundled agent directories.
pub fn bundled_agent_names() -> Vec<String> {
    BUNDLED_AGENTS
        .dirs()
        .filter_map(|d| d.path().to_str().map(str::to_string))
        .collect()
}
