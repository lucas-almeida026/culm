//! The git boundary. Worktree creation is the only version control work culm does.

use std::fmt;
use std::path::Path;
use std::process::Command;

use anyhow::{Result, bail};

pub trait Git: fmt::Debug {
    /// Creates a worktree at `path` on a new branch `branch`, taken from the
    /// repository at `repo`. Does nothing when `path` already exists, so reopening a
    /// project never fails on a worktree that is already there.
    fn worktree_add(&self, repo: &Path, path: &Path, branch: &str) -> Result<()>;

    /// True when the worktree holds a change that is not committed.
    fn worktree_is_dirty(&self, path: &Path) -> Result<bool>;

    /// Removes a worktree directory. Never removes its branch.
    fn worktree_remove(&self, repo: &Path, path: &Path) -> Result<()>;
}

/// Runs the real `git` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemGit;

impl Git for SystemGit {
    fn worktree_add(&self, repo: &Path, path: &Path, branch: &str) -> Result<()> {
        if path.exists() {
            return Ok(());
        }
        let out = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["worktree", "add", "-b"])
            .arg(branch)
            .arg(path)
            .output()?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            bail!("{}: {}", repo.display(), stderr.trim().replace('\n', " "));
        }
        Ok(())
    }

    fn worktree_is_dirty(&self, path: &Path) -> Result<bool> {
        if !path.exists() {
            return Ok(false);
        }
        let out = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(["status", "--porcelain"])
            .output()?;
        Ok(!out.stdout.is_empty())
    }

    fn worktree_remove(&self, repo: &Path, path: &Path) -> Result<()> {
        if !path.exists() {
            return Ok(());
        }
        // The user already confirmed, and the prompt named every dirty worktree, so
        // git is told not to refuse. The branch is never touched.
        let out = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["worktree", "remove", "--force"])
            .arg(path)
            .output()?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            bail!("{}: {}", path.display(), stderr.trim().replace('\n', " "));
        }
        Ok(())
    }
}
