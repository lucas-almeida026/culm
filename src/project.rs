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
    /// Unix seconds of the last interaction. Orders the paused list, newest first.
    /// A record written before culm tracked this reads as zero and sorts last.
    #[serde(default)]
    pub last_active: u64,
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

/// The directory name the Claude Code CLI gives a working directory under
/// `~/.claude/projects`. Every `/` and every `.` becomes `-`.
#[must_use]
pub fn transcript_dir_name(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect()
}

/// True when a directory under `~/.claude/projects` belongs to this project.
///
/// The directory is the encoded working directory. A match is the root itself, or,
/// when `recursive`, any path under it. The separator is required, so `/home/x/spm2`
/// never matches `/home/x/spm`.
#[must_use]
pub fn dir_belongs_to(root: &Path, dir_name: &str, recursive: bool) -> bool {
    let encoded = transcript_dir_name(root);
    dir_name == encoded || (recursive && dir_name.starts_with(&format!("{encoded}-")))
}

/// Everything one `culm project rm` removes outside its own state file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Removal {
    /// Directory names under `~/.claude/projects`.
    pub claude_dirs: Vec<String>,
    /// The repository each worktree belongs to, and the worktree itself.
    pub worktrees: Vec<(PathBuf, PathBuf)>,
}

/// Decides what a removal touches. Pure, so the destructive set is testable.
///
/// `claude_dirs` is the directory listing of `~/.claude/projects`. A directory
/// belongs to the project when it is the root itself, or sits under the root. The
/// separator is required, so `/home/x/spm2` never matches `/home/x/spm`.
#[must_use]
pub fn plan_removal(project: &Project, claude_dirs: &[String], recursive: bool) -> Removal {
    let claude_dirs = claude_dirs
        .iter()
        .filter(|name| dir_belongs_to(&project.root, name, recursive))
        .cloned()
        .collect();

    let mut worktrees = Vec::new();
    if recursive {
        for session in &project.sessions {
            for repo in &session.repos {
                if let Some(known) = project.repo(&repo.name) {
                    worktrees.push((known.path.clone(), repo.worktree.clone()));
                }
            }
        }
    }
    Removal {
        claude_dirs,
        worktrees,
    }
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
    fn transcript_dir_name_replaces_slashes_and_dots() {
        assert_eq!(
            transcript_dir_name(Path::new("/home/x/Documents/GitHub/culm")),
            "-home-x-Documents-GitHub-culm"
        );
        assert_eq!(
            transcript_dir_name(Path::new("/home/x/.config/i3")),
            "-home-x--config-i3",
            "a dot becomes a dash, so .config yields two dashes"
        );
    }

    fn project_with_a_session() -> Project {
        let mut project = Project::new("spm", "/home/x/spm");
        project.repos = vec![Repository {
            name: "api-mate".into(),
            path: PathBuf::from("/home/x/spm/api-mate"),
        }];
        project.sessions = vec![SessionRecord {
            id: "id-1".into(),
            name: "feat A".into(),
            slug: "feat-a".into(),
            state: SessionState::Paused,
            cwd: PathBuf::from("/home/x/spm/.worktrees/api-mate-feat-a"),
            repos: vec![SessionRepo {
                name: "api-mate".into(),
                worktree: PathBuf::from("/home/x/spm/.worktrees/api-mate-feat-a"),
                branch: "feat-a".into(),
            }],
            started: true,
            last_active: 0,
        }];
        project
    }

    #[test]
    fn a_plain_removal_takes_the_root_directory_only() {
        let dirs = vec![
            "-home-x-spm".to_string(),
            "-home-x-spm--worktrees-api-mate-feat-a".to_string(),
        ];
        let plan = plan_removal(&project_with_a_session(), &dirs, false);
        assert_eq!(plan.claude_dirs, vec!["-home-x-spm".to_string()]);
        assert!(plan.worktrees.is_empty(), "a worktree needs --recursive");
    }

    #[test]
    fn a_recursive_removal_takes_every_directory_under_the_root() {
        let dirs = vec![
            "-home-x-spm".to_string(),
            "-home-x-spm--worktrees-api-mate-feat-a".to_string(),
        ];
        let plan = plan_removal(&project_with_a_session(), &dirs, true);
        assert_eq!(plan.claude_dirs.len(), 2);
        assert_eq!(
            plan.worktrees,
            vec![(
                PathBuf::from("/home/x/spm/api-mate"),
                PathBuf::from("/home/x/spm/.worktrees/api-mate-feat-a")
            )]
        );
    }

    #[test]
    fn a_sibling_project_with_a_longer_name_is_never_taken() {
        let dirs = vec!["-home-x-spm".to_string(), "-home-x-spm2".to_string()];
        let plan = plan_removal(&project_with_a_session(), &dirs, true);
        assert_eq!(
            plan.claude_dirs,
            vec!["-home-x-spm".to_string()],
            "spm2 is a different project, not a directory under spm"
        );
    }

    #[test]
    fn a_worktree_of_an_unregistered_repository_is_left_alone() {
        let mut project = project_with_a_session();
        project.repos.clear();
        let plan = plan_removal(&project, &[], true);
        assert!(plan.worktrees.is_empty());
    }

    #[test]
    fn a_directory_belongs_to_the_root_only_across_a_separator() {
        let root = Path::new("/home/x/spm");
        assert!(dir_belongs_to(root, "-home-x-spm", false));
        assert!(!dir_belongs_to(root, "-home-x-spm-api-mate", false));
        assert!(dir_belongs_to(root, "-home-x-spm-api-mate", true));
        assert!(!dir_belongs_to(root, "-home-x-spm2", true));
        assert!(!dir_belongs_to(root, "-home-x-other", true));
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
