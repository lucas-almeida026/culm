//! Project data. Pure values and pure functions, so every rule here is unit tested.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A git repository that belongs to a project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repository {
    pub name: String,
    pub path: PathBuf,
}

/// Whether a session holds a running process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionState {
    Active,
    Paused,
}

/// One repository that a session edits, and the worktree it edits it through.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRepo {
    pub name: String,
    pub worktree: PathBuf,
    pub branch: String,
}

/// Everything culm must remember about a session across a restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    /// The Claude Code session id. culm generates it and passes `--session-id`.
    pub id: String,
    pub name: String,
    pub slug: String,
    pub state: SessionState,
    pub cwd: PathBuf,
    pub repos: Vec<SessionRepo>,
    /// True once the session started at least once. A later start uses `--resume`.
    #[serde(default)]
    pub started: bool,
}

impl SessionRecord {
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.state == SessionState::Active
    }
}

/// One managed directory, its repositories, and its sessions.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub slug: String,
    pub root: PathBuf,
    #[serde(default)]
    pub repos: Vec<Repository>,
    #[serde(default)]
    pub sessions: Vec<SessionRecord>,
}

impl Project {
    #[must_use]
    pub fn new(slug: impl Into<String>, root: impl Into<PathBuf>) -> Self {
        Self {
            slug: slug.into(),
            root: root.into(),
            repos: Vec::new(),
            sessions: Vec::new(),
        }
    }

    #[must_use]
    pub fn repo(&self, name: &str) -> Option<&Repository> {
        self.repos.iter().find(|r| r.name == name)
    }

    /// The directory of the worktree for one repository of one session.
    #[must_use]
    pub fn worktree_dir(&self, repo: &str, session_slug: &str) -> PathBuf {
        worktree_dir(&self.root, repo, session_slug)
    }
}

/// One entry of the global registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectEntry {
    pub slug: String,
    pub root: PathBuf,
}

/// The global project list. culm opens any project from any working directory.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registry {
    #[serde(default)]
    pub projects: Vec<ProjectEntry>,
}

impl Registry {
    #[must_use]
    pub fn find(&self, slug: &str) -> Option<&ProjectEntry> {
        self.projects.iter().find(|p| p.slug == slug)
    }

    #[must_use]
    pub fn holds_root(&self, root: &Path) -> Option<&ProjectEntry> {
        self.projects.iter().find(|p| p.root == root)
    }

    /// The registered project that owns `cwd`. The longest matching root wins, so a
    /// project nested inside another project still resolves to itself.
    #[must_use]
    pub fn owner_of(&self, cwd: &Path) -> Option<&ProjectEntry> {
        self.projects
            .iter()
            .filter(|p| cwd.starts_with(&p.root))
            .max_by_key(|p| p.root.as_os_str().len())
    }

    /// Registers `root` under a slug that no other project holds.
    pub fn add(&mut self, root: impl Into<PathBuf>) -> ProjectEntry {
        let root = root.into();
        let base = root
            .file_name()
            .map(|n| slugify(&n.to_string_lossy()))
            .unwrap_or_else(|| "project".to_string());
        let taken: Vec<&str> = self.projects.iter().map(|p| p.slug.as_str()).collect();
        let entry = ProjectEntry {
            slug: unique_slug(&base, &taken),
            root,
        };
        self.projects.push(entry.clone());
        entry
    }
}

/// Turns a name into a slug that is safe in a path, a branch name, and a file name.
#[must_use]
pub fn slugify(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "session".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Appends a counter until the slug is free.
#[must_use]
pub fn unique_slug(base: &str, taken: &[&str]) -> String {
    if !taken.contains(&base) {
        return base.to_string();
    }
    for n in 2.. {
        let candidate = format!("{base}-{n}");
        if !taken.contains(&candidate.as_str()) {
            return candidate;
        }
    }
    base.to_string()
}

/// Every worktree lives under the project root, prefixed by its repository name, so
/// that two repositories with the same session slug keep distinct directories.
#[must_use]
pub fn worktree_dir(root: &Path, repo: &str, session_slug: &str) -> PathBuf {
    root.join(".worktrees")
        .join(format!("{repo}-{session_slug}"))
}

/// The system prompt addition that maps each repository to its worktree.
///
/// This is convention only. Nothing enforces it, and a model decides whether to
/// follow the mapping.
#[must_use]
pub fn system_prompt(repos: &[SessionRepo]) -> String {
    if repos.is_empty() {
        return String::new();
    }
    let mut s = String::from(
        "culm manages this session. Each repository below is checked out as a git worktree. \
         Edit files through the worktree path, never through the original checkout.\n",
    );
    for r in repos {
        s.push_str(&format!(
            "- {} -> {} (branch {})\n",
            r.name,
            r.worktree.display(),
            r.branch
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_lowercases_and_dashes() {
        assert_eq!(slugify("Feat A"), "feat-a");
        assert_eq!(slugify("CLUB-1020 fix"), "club-1020-fix");
        assert_eq!(slugify("  spaced  "), "spaced");
        assert_eq!(slugify("!!!"), "session");
        assert_eq!(slugify("já_foi"), "j-foi");
    }

    #[test]
    fn unique_slug_appends_a_counter() {
        assert_eq!(unique_slug("spm", &[]), "spm");
        assert_eq!(unique_slug("spm", &["spm"]), "spm-2");
        assert_eq!(unique_slug("spm", &["spm", "spm-2"]), "spm-3");
    }

    #[test]
    fn worktree_path_prefixes_the_repository_name() {
        let p = worktree_dir(Path::new("/home/x/spm"), "api-mate", "feat-a");
        assert_eq!(p, Path::new("/home/x/spm/.worktrees/api-mate-feat-a"));
    }

    #[test]
    fn two_repositories_of_one_session_keep_distinct_worktrees() {
        let root = Path::new("/home/x/spm");
        assert_ne!(
            worktree_dir(root, "api-mate", "feat-a"),
            worktree_dir(root, "crm-mate", "feat-a")
        );
    }

    #[test]
    fn the_owning_project_is_the_longest_matching_root() {
        let mut r = Registry::default();
        r.add("/home/x/spm");
        r.add("/home/x/spm/inner");
        let owner = r.owner_of(Path::new("/home/x/spm/inner/deep"));
        assert_eq!(
            owner.map(|p| p.root.as_path()),
            Some(Path::new("/home/x/spm/inner"))
        );
    }

    #[test]
    fn a_directory_outside_every_root_owns_no_project() {
        let mut r = Registry::default();
        r.add("/home/x/spm");
        assert!(r.owner_of(Path::new("/home/y")).is_none());
    }

    #[test]
    fn a_second_project_with_the_same_directory_name_gets_a_free_slug() {
        let mut r = Registry::default();
        assert_eq!(r.add("/a/spm").slug, "spm");
        assert_eq!(r.add("/b/spm").slug, "spm-2");
    }

    #[test]
    fn the_system_prompt_names_every_worktree_and_branch() {
        let repos = vec![SessionRepo {
            name: "api-mate".into(),
            worktree: PathBuf::from("/home/x/spm/.worktrees/api-mate-feat-a"),
            branch: "feat-a".into(),
        }];
        let s = system_prompt(&repos);
        assert!(s.contains("/home/x/spm/.worktrees/api-mate-feat-a"));
        assert!(s.contains("branch feat-a"));
    }

    #[test]
    fn a_session_with_no_repository_gets_no_system_prompt() {
        assert_eq!(system_prompt(&[]), "");
    }
}
