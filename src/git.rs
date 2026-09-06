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
}
