use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Validates that a path is within the allowed sandbox directory.
/// Returns the canonicalized path if valid.
pub fn validate_sandbox_path(sandbox_dir: &Path, requested: &str) -> Result<PathBuf> {
    let sandbox_canonical = sandbox_dir
        .canonicalize()
        .with_context(|| format!("Sandbox directory not found: {}", sandbox_dir.display()))?;

    let requested_path = if Path::new(requested).is_absolute() {
        PathBuf::from(requested)
    } else {
        sandbox_dir.join(requested)
    };

    // For paths that don't exist yet (write_file), check the parent
    let check_path = if requested_path.exists() {
        requested_path
            .canonicalize()
            .context("Failed to canonicalize path")?
    } else {
        let parent = requested_path
            .parent()
            .context("Path has no parent directory")?;
        let parent_canonical = parent
            .canonicalize()
            .with_context(|| format!("Parent directory not found: {}", parent.display()))?;
        parent_canonical.join(requested_path.file_name().context("Path has no filename")?)
    };

    if !check_path.starts_with(&sandbox_canonical) {
        anyhow::bail!(
            "Access denied: path '{}' is outside the sandbox directory '{}'",
            requested,
            sandbox_dir.display()
        );
    }

    Ok(check_path)
}

/// Validates that a path is within the RustFox home directory.
/// Returns the canonicalized path if valid.
pub fn validate_home_path(home_dir: &Path, requested: &str) -> Result<PathBuf> {
    let home_canonical = home_dir
        .canonicalize()
        .with_context(|| format!("Home directory not found: {}", home_dir.display()))?;

    let requested_path = if Path::new(requested).is_absolute() {
        PathBuf::from(requested)
    } else {
        home_dir.join(requested)
    };

    let check_path = if requested_path.exists() {
        requested_path
            .canonicalize()
            .context("Failed to canonicalize path")?
    } else {
        let parent = requested_path
            .parent()
            .context("Path has no parent directory")?;
        let parent_canonical = parent
            .canonicalize()
            .with_context(|| format!("Parent directory not found: {}", parent.display()))?;
        parent_canonical.join(requested_path.file_name().context("Path has no filename")?)
    };

    if !check_path.starts_with(&home_canonical) {
        anyhow::bail!(
            "Access denied: path '{}' is outside the home directory '{}'",
            requested,
            home_dir.display()
        );
    }

    Ok(check_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_validate_home_path_allows_home_files() {
        let dir = tempdir().unwrap();
        let home = dir.path().join(".rustfox");
        std::fs::create_dir_all(&home).unwrap();
        let soul = home.join("SOUL.md");
        std::fs::write(&soul, "# Soul").unwrap();

        let result = validate_home_path(&home, "SOUL.md").unwrap();
        assert_eq!(result, soul.canonicalize().unwrap());
    }

    #[test]
    fn test_validate_home_path_denies_outside() {
        let dir = tempdir().unwrap();
        let home = dir.path().join(".rustfox");
        std::fs::create_dir_all(&home).unwrap();
        let outside = dir.path().join("outside.txt");
        std::fs::write(&outside, "data").unwrap();

        let result = validate_home_path(&home, "../outside.txt");
        assert!(result.is_err());
    }
}
