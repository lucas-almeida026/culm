//! The git boundary. culm only reads: what a session changed, and what a worktree a
//! session registers is. It never writes to a repository.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Result, bail};

pub trait Git: fmt::Debug {
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

    /// What the checkout that holds `path` is: its top directory, the repository it
    /// belongs to, its branch, and whether it is a linked worktree. `None` when no
    /// repository holds `path`.
    fn worktree_info(&self, path: &Path) -> Result<Option<WorktreeInfo>>;
}

/// One checkout, as `Git::worktree_info` reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeInfo {
    pub top: PathBuf,
    /// The directory name of the main checkout, which names the repository for every
    /// worktree of it.
    pub repo: String,
    /// Empty when `HEAD` is detached.
    pub branch: String,
    /// True for a worktree made by `git worktree add`, false for a main checkout.
    pub linked: bool,
}

/// The repository name for a git common directory: the directory that holds `.git`,
/// or the bare repository itself without its `.git` suffix.
#[must_use]
pub fn repo_name(common_dir: &Path) -> String {
    let dir = if common_dir.file_name().is_some_and(|n| n == ".git") {
        common_dir.parent().unwrap_or(common_dir)
    } else {
        common_dir
    };
    let name = dir.file_name().map_or_else(
        || dir.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    name.strip_suffix(".git")
        .map_or(name.clone(), str::to_string)
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

    fn worktree_info(&self, path: &Path) -> Result<Option<WorktreeInfo>> {
        let Some(dir) = path.ancestors().find(|p| p.is_dir()) else {
            return Ok(None);
        };
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "rev-parse",
                "--path-format=absolute",
                "--show-toplevel",
                "--git-dir",
                "--git-common-dir",
            ])
            .output()?;
        if !out.status.success() {
            return Ok(None);
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let mut lines = text.lines().map(str::trim);
        let (Some(top), Some(git_dir), Some(common)) = (lines.next(), lines.next(), lines.next())
        else {
            return Ok(None);
        };
        let top = PathBuf::from(top);
        let branch = read(&top, &["branch", "--show-current"])
            .map(|b| String::from_utf8_lossy(&b).trim().to_string())
            .unwrap_or_default();
        Ok(Some(WorktreeInfo {
            repo: repo_name(Path::new(common)),
            linked: git_dir != common,
            top,
            branch,
        }))
    }

    fn working_file(&self, worktree: &Path, path: &str) -> Result<Option<Vec<u8>>> {
        match std::fs::read(worktree.join(path)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_checkout_is_named_by_the_directory_that_holds_its_git_directory() {
        assert_eq!(
            repo_name(Path::new("/home/x/spm/api-mate/.git")),
            "api-mate",
            "every worktree of api-mate shares this common directory"
        );
    }

    #[test]
    fn a_bare_repository_is_named_without_its_suffix() {
        assert_eq!(repo_name(Path::new("/srv/git/api-mate.git")), "api-mate");
    }
}
