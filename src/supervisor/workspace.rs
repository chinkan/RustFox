use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tokio::process::Command;

pub struct Workspace {
    pub path: PathBuf,
    pub branch: String,
}

pub struct WorkspaceManager {
    repo: PathBuf,
    use_worktree: bool,
}

impl WorkspaceManager {
    pub fn new(repo: PathBuf, use_worktree: bool) -> Self {
        Self { repo, use_worktree }
    }

    pub async fn prepare(&self, task_id: &str, slug: &str) -> Result<Workspace> {
        let safe_slug: String = slug
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        let branch = format!("supervisor/{safe_slug}-{}", &task_id[..8]);

        if self.use_worktree {
            let repo_name = self
                .repo
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let path = self
                .repo
                .parent()
                .unwrap_or(&self.repo)
                .join(format!("{repo_name}-worktree-{}", &task_id[..8]));
            run(
                &self.repo,
                &["worktree", "add", "-b", &branch, path.to_str().unwrap()],
            )
            .await
            .context("git worktree add")?;
            Ok(Workspace { path, branch })
        } else {
            run(&self.repo, &["checkout", "-b", &branch])
                .await
                .context("git checkout -b")?;
            Ok(Workspace {
                path: self.repo.clone(),
                branch,
            })
        }
    }

    pub async fn cleanup(&self, ws: &Workspace, keep_branch: bool) -> Result<()> {
        if self.use_worktree {
            run(
                &self.repo,
                &["worktree", "remove", ws.path.to_str().unwrap(), "--force"],
            )
            .await?;
        }
        if !keep_branch {
            run(&self.repo, &["branch", "-D", &ws.branch]).await.ok();
        }
        Ok(())
    }
}

async fn run(cwd: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .await?;
    if !out.status.success() {
        anyhow::bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn init_git_repo(p: &std::path::Path) {
        let run = |args: &[&str]| {
            let mut cmd = std::process::Command::new("git");
            cmd.args(args).current_dir(p);
            cmd.env("GIT_AUTHOR_NAME", "test")
                .env("GIT_AUTHOR_EMAIL", "test@example.com");
            cmd.env("GIT_COMMITTER_NAME", "test")
                .env("GIT_COMMITTER_EMAIL", "test@example.com");
            let _ = cmd.output().expect("git command");
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "test"]);
        tokio::fs::write(p.join("README.md"), "init").await.unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "init"]);
    }

    async fn git(p: &std::path::Path, args: &[&str]) -> String {
        let out = tokio::process::Command::new("git")
            .args(args)
            .current_dir(p)
            .output()
            .await
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    #[tokio::test]
    async fn creates_branch_in_existing_repo() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path()).await;
        let wm = WorkspaceManager::new(dir.path().into(), false);
        let ws = wm.prepare("task-abc", "fix-login-bug").await.unwrap();
        assert!(ws.branch.starts_with("supervisor/"));
        assert_eq!(ws.path, dir.path());
        let branches = git(dir.path(), &["branch", "--show-current"]).await;
        assert_eq!(branches.trim(), ws.branch);
    }

    #[tokio::test]
    async fn creates_worktree_when_requested() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path()).await;
        let wm = WorkspaceManager::new(dir.path().into(), true);
        let ws = wm.prepare("task-xyz", "refactor-foo").await.unwrap();
        assert_ne!(ws.path, dir.path());
        assert!(ws.path.exists());
    }
}
