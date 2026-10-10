//! The git boundary. culm creates and removes worktrees, and reads a worktree to
//! show its changes. It never writes to a repository in any other way.

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

    /// `git status --porcelain=v2 -z --untracked-files=all`, as git wrote it.
    fn status(&self, worktree: &Path) -> Result<Vec<u8>>;

    /// The newest `limit` commits of `HEAD`, one record per commit, ended by NUL.
    /// The fields are the hash, the short hash, the author, the Unix time of the
    /// author date, and the subject, separated by the unit separator `0x1f`.
    fn log(&self, worktree: &Path, limit: usize) -> Result<Vec<u8>>;

    /// The files one commit changed, against its first parent, as
    /// `--name-status -z` writes them.
    fn commit_files(&self, worktree: &Path, sha: &str) -> Result<Vec<u8>>;

    /// The content of `path` at `rev`. `None` when the revision holds no such file,
    /// or when the revision does not exist, such as the parent of a root commit.
    fn blob(&self, worktree: &Path, rev: &str, path: &str) -> Result<Option<Vec<u8>>>;

    /// The content of `path` in the working tree. `None` when the file is gone.
    fn working_file(&self, worktree: &Path, path: &str) -> Result<Option<Vec<u8>>>;

    /// The top directory of the repository or worktree that holds `path`. `None`
    /// when no repository holds it. A path that no longer exists is looked up from
    /// its nearest directory that does.
    fn toplevel(&self, path: &Path) -> Result<Option<std::path::PathBuf>>;
}

/// The format `log` asks git for. Kept beside the parser's expectations.
pub const LOG_FORMAT: &str = "--format=%H%x1f%h%x1f%an%x1f%at%x1f%s";

/// Runs git in `worktree` and returns its output, or fails with its error text.
fn read(worktree: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(args)
        .output()?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("{}", stderr.trim().replace('\n', " "));
    }
    Ok(out.stdout)
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

    fn status(&self, worktree: &Path) -> Result<Vec<u8>> {
        read(
            worktree,
            &["status", "--porcelain=v2", "-z", "--untracked-files=all"],
        )
    }

    fn log(&self, worktree: &Path, limit: usize) -> Result<Vec<u8>> {
        let limit = format!("-n{limit}");
        match read(worktree, &["log", "-z", &limit, LOG_FORMAT]) {
            Ok(out) => Ok(out),
            // A repository with no commit yet has no history, which is not an error.
            Err(e) if e.to_string().contains("does not have any commits") => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    fn commit_files(&self, worktree: &Path, sha: &str) -> Result<Vec<u8>> {
        read(
            worktree,
            &[
                "show",
                "--diff-merges=first-parent",
                "-M",
                "--name-status",
                "-z",
                "--format=",
                sha,
            ],
        )
    }

    fn blob(&self, worktree: &Path, rev: &str, path: &str) -> Result<Option<Vec<u8>>> {
        let spec = format!("{rev}:{path}");
        let out = Command::new("git")
            .arg("-C")
            .arg(worktree)
            .args(["cat-file", "blob", &spec])
            .output()?;
        // git answers a missing file and a missing revision the same way, and both
        // mean the side of the diff is empty.
        Ok(out.status.success().then_some(out.stdout))
    }

    fn toplevel(&self, path: &Path) -> Result<Option<std::path::PathBuf>> {
        let Some(dir) = path.ancestors().find(|p| p.is_dir()) else {
            return Ok(None);
        };
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["rev-parse", "--show-toplevel"])
            .output()?;
        if !out.status.success() {
            return Ok(None);
        }
        let top = String::from_utf8_lossy(&out.stdout).trim().to_string();
        Ok((!top.is_empty()).then(|| std::path::PathBuf::from(top)))
    }

    fn working_file(&self, worktree: &Path, path: &str) -> Result<Option<Vec<u8>>> {
        match std::fs::read(worktree.join(path)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}
