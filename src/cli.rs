//! The command line. Every project operation is reachable without the interface, so
//! a shell script reaches culm too.

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};
use clap::{Parser, Subcommand};

use crate::git::Git;
use crate::hooks;
use crate::namer::Namer;
use crate::project::{
    Project, ProjectEntry, Registry, Removal, Repository, SessionRecord, SessionState,
    dir_belongs_to, plan_removal, slugify, transcript_dir_name, unique_slug,
};
use crate::store::Store;
use crate::transcript;

#[derive(Debug, Parser)]
#[command(
    name = "culm",
    version,
    about = "Many Claude Code sessions in one project, as vertical tabs."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Open a project in the interface.
    Open {
        /// Project slug. Defaults to the project that owns the working directory.
        project: Option<String>,
        /// Import every Claude Code session found under the root before opening.
        #[arg(long)]
        import_native_sessions: bool,
    },
    /// Register projects and their repositories.
    #[command(subcommand)]
    Project(ProjectCmd),
    /// Install or remove the Claude Code hook entries that feed attention markers.
    #[command(subcommand)]
    Hooks(HooksCmd),
    /// Hook client. Reads one payload on stdin and posts it to the interface.
    #[command(hide = true)]
    Hook,
}

#[derive(Debug, Subcommand)]
pub enum ProjectCmd {
    /// Register a directory as a project.
    New {
        /// Any directory. A git repository at the root is not required.
        path: PathBuf,
        /// Name the project. Without this, the name comes from the last part of the
        /// path.
        #[arg(long)]
        name: Option<String>,
        /// Import every Claude Code session found under the root.
        #[arg(long)]
        import_native_sessions: bool,
    },
    /// List every registered project.
    List,
    /// Import every Claude Code session found under the root, as paused sessions.
    Import {
        #[arg(long)]
        project: Option<String>,
    },
    /// Remove a project. Never deletes a branch, a source file, or a repository.
    Rm {
        project: String,
        /// Skip the confirmation. Widens nothing.
        #[arg(long)]
        force: bool,
        /// Also remove the Claude Code data of every path under the root, and every
        /// worktree the project created.
        #[arg(long)]
        recursive: bool,
    },
    /// Change a property of a project.
    #[command(subcommand)]
    Alter(AlterCmd),
    /// List, add, or remove a repository of a project.
    #[command(subcommand)]
    Repo(RepoCmd),
}

#[derive(Debug, Subcommand)]
pub enum AlterCmd {
    /// Rename a project. `culm open <name>` takes the new name from then on.
    Name {
        name: String,
        /// Project to rename. Defaults to the project that owns the working
        /// directory.
        #[arg(long)]
        project: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum RepoCmd {
    /// Add a git repository to a project.
    Add {
        path: PathBuf,
        #[arg(long)]
        project: Option<String>,
    },
    /// Remove a repository from a project. The repository itself is not deleted.
    Rm {
        name: String,
        #[arg(long)]
        project: Option<String>,
    },
    /// List the repositories of a project.
    List {
        #[arg(long)]
        project: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum HooksCmd {
    /// Route the Claude Code hooks to this binary.
    Install,
    /// Remove the culm entries and leave every other entry untouched.
    Uninstall,
}

/// What the caller must do after the command ran.
#[derive(Debug)]
pub enum Outcome {
    /// Open this project in the interface.
    Open(Box<Project>),
    /// The command is finished.
    Done,
}

/// Runs one command. Printing happens here, because a command line is output.
pub fn run(
    cli: Cli,
    store: &dyn Store,
    git: &dyn Git,
    namer: &dyn Namer,
    exe: &Path,
    cwd: &Path,
) -> Result<Outcome> {
    match cli.command {
        None => open(store, namer, None, cwd, false),
        Some(Command::Open {
            project,
            import_native_sessions,
        }) => open(
            store,
            namer,
            project.as_deref(),
            cwd,
            import_native_sessions,
        ),
        Some(Command::Hook) => {
            hooks::send_from_stdin();
            Ok(Outcome::Done)
        }
        Some(Command::Hooks(HooksCmd::Install)) => {
            let settings = store.read_claude_settings()?;
            store.write_claude_settings(&hooks::install(&settings, exe))?;
            println!("hooks installed for {}", exe.display());
            Ok(Outcome::Done)
        }
        Some(Command::Hooks(HooksCmd::Uninstall)) => {
            let settings = store.read_claude_settings()?;
            store.write_claude_settings(&hooks::uninstall(&settings))?;
            println!("hooks removed");
            Ok(Outcome::Done)
        }
        Some(Command::Project(ProjectCmd::New {
            path,
            name,
            import_native_sessions,
        })) => project_new(store, namer, &path, name.as_deref(), import_native_sessions),
        Some(Command::Project(ProjectCmd::Alter(AlterCmd::Name { name, project }))) => {
            project_rename(store, &name, project.as_deref(), cwd)
        }
        Some(Command::Project(ProjectCmd::List)) => project_list(store),
        Some(Command::Project(ProjectCmd::Import { project })) => {
            project_import_cmd(store, namer, project.as_deref(), cwd)
        }
        Some(Command::Project(ProjectCmd::Rm {
            project,
            force,
            recursive,
        })) => project_rm(store, git, &project, force, recursive),
        Some(Command::Project(ProjectCmd::Repo(RepoCmd::Add { path, project }))) => {
            repo_add(store, &path, project.as_deref(), cwd)
        }
        Some(Command::Project(ProjectCmd::Repo(RepoCmd::Rm { name, project }))) => {
            repo_rm(store, &name, project.as_deref(), cwd)
        }
        Some(Command::Project(ProjectCmd::Repo(RepoCmd::List { project }))) => {
            repo_list(store, project.as_deref(), cwd)
        }
    }
}

/// The slug a project name becomes.
///
/// The slug is the identifier `culm open` takes and the name of the state file, so it
/// carries no space and no separator.
fn project_slug(name: &str) -> Result<String> {
    if name.trim().is_empty() {
        bail!("a project needs a name");
    }
    Ok(slugify(name))
}

fn project_new(
    store: &dyn Store,
    namer: &dyn Namer,
    path: &Path,
    name: Option<&str>,
    import: bool,
) -> Result<Outcome> {
    let root = std::fs::canonicalize(path).map_err(|e| anyhow!("{}: {e}", path.display()))?;
    if !root.is_dir() {
        bail!("{} is not a directory", root.display());
    }
    let mut registry = store.load_registry()?;
    if let Some(existing) = registry.holds_root(&root) {
        bail!(
            "{} is already the project {}",
            root.display(),
            existing.slug
        );
    }
    let entry = match name {
        Some(name) => {
            let slug = project_slug(name)?;
            registry.add_as(&root, &slug).ok_or_else(|| {
                anyhow!("a project named {slug} already exists. run: culm project list")
            })?
        }
        None => registry.add(&root),
    };
    store.save_registry(&registry)?;
    let mut project = Project::new(&entry.slug, &root);
    store.save_project(&project)?;
    println!("project {} at {}", entry.slug, root.display());
    if import {
        report(&import_sessions(store, namer, &mut project)?);
    }
    Ok(Outcome::Done)
}

/// Renames a project, and moves its saved state to the new name.
///
/// The state file is written under the new name before the registry points at it, so
/// an interrupted rename leaves the old name working rather than leaving a registered
/// project with no state.
fn project_rename(
    store: &dyn Store,
    new_name: &str,
    slug: Option<&str>,
    cwd: &Path,
) -> Result<Outcome> {
    let mut registry = store.load_registry()?;
    let entry = find_entry(&registry, slug, cwd)?.clone();
    let new_slug = project_slug(new_name)?;
    if new_slug == entry.slug {
        println!("project {} already has that name", entry.slug);
        return Ok(Outcome::Done);
    }
    if registry.rename(&entry.slug, &new_slug).is_none() {
        bail!("a project named {new_slug} already exists. run: culm project list");
    }

    let mut project = load_or_new(store, &entry)?;
    project.slug = new_slug.clone();
    store.save_project(&project)?;
    store.save_registry(&registry)?;
    store.remove_project(&entry.slug)?;
    println!("project {} is now {new_slug}", entry.slug);
    println!("an interface already open on it keeps the old name until it closes.");
    Ok(Outcome::Done)
}

fn project_import_cmd(
    store: &dyn Store,
    namer: &dyn Namer,
    slug: Option<&str>,
    cwd: &Path,
) -> Result<Outcome> {
    let mut project = resolve(store, slug, cwd)?;
    report(&import_sessions(store, namer, &mut project)?);
    Ok(Outcome::Done)
}

/// What one import did. Printing is the caller's job, so the decision stays testable.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ImportReport {
    pub imported: usize,
    /// How many names came from the namer rather than from the transcript.
    pub named: usize,
    /// One line per transcript that was left alone, and why.
    pub skipped: Vec<String>,
}

fn report(report: &ImportReport) {
    println!(
        "imported {} sessions ({} named by haiku, {} skipped)",
        report.imported,
        report.named,
        report.skipped.len()
    );
    for line in &report.skipped {
        println!("  skipped {line}");
    }
}

/// The name a transcript falls back to when nothing describes it.
fn fallback_name(id: &str) -> String {
    format!("session-{}", id.chars().take(8).collect::<String>())
}

/// Pulls every Claude Code transcript under the root into the project as a paused
/// session.
///
/// The session id is kept, so `--resume` reaches the same conversation. A transcript
/// whose id the project already holds is left alone, which makes a second import add
/// only what appeared since the first.
pub fn import_sessions(
    store: &dyn Store,
    namer: &dyn Namer,
    project: &mut Project,
) -> Result<ImportReport> {
    let mut report = ImportReport::default();
    let root_dir = transcript_dir_name(&project.root);
    let mut known: Vec<String> = project.sessions.iter().map(|s| s.id.clone()).collect();
    let mut taken: Vec<String> = project.sessions.iter().map(|s| s.slug.clone()).collect();

    let mut dirs = store.list_claude_dirs()?;
    dirs.sort();
    for dir in dirs {
        if !dir_belongs_to(&project.root, &dir, true) {
            continue;
        }
        let mut files = store.list_transcripts(&dir)?;
        // Newest first, so a name collision numbers the older session rather than the
        // one the user just left.
        files.sort_by(|a, b| b.modified_secs.cmp(&a.modified_secs).then(a.id.cmp(&b.id)));
        for file in files {
            if known.contains(&file.id) {
                continue;
            }
            let head = transcript::scan(&file.id, &store.read_transcript(&dir, &file.id)?);
            if head.custom_title.is_none() && head.first_prompt.is_none() {
                report
                    .skipped
                    .push(format!("{}: nothing to name it by", file.id));
                continue;
            }
            // `--resume` reads the transcript from the directory it was written in, so
            // a session with no recorded working directory cannot be resumed.
            let cwd = match head.cwd.clone() {
                Some(cwd) => cwd,
                None if dir == root_dir => project.root.clone(),
                None => {
                    report
                        .skipped
                        .push(format!("{}: no working directory recorded", file.id));
                    continue;
                }
            };
            let base = match head.custom_title.as_deref() {
                Some(title) => slugify(title),
                None => match head.first_prompt.as_deref() {
                    Some(prompt) => match namer.name_for(prompt) {
                        Ok(name) => {
                            report.named += 1;
                            slugify(&name)
                        }
                        Err(e) => {
                            report
                                .skipped
                                .push(format!("{}: named from its id, because {e}", file.id));
                            fallback_name(&file.id)
                        }
                    },
                    None => fallback_name(&file.id),
                },
            };
            let slug = unique_slug(&base, &taken.iter().map(String::as_str).collect::<Vec<_>>());
            taken.push(slug.clone());
            known.push(file.id.clone());
            project.sessions.push(SessionRecord {
                id: file.id.clone(),
                name: slug.clone(),
                slug,
                state: SessionState::Paused,
                cwd,
                repos: Vec::new(),
                // The transcript exists, so the session has run and resumes.
                started: true,
                last_active: file.modified_secs,
            });
            report.imported += 1;
        }
    }
    store.save_project(project)?;
    Ok(report)
}

fn project_list(store: &dyn Store) -> Result<Outcome> {
    let registry = store.load_registry()?;
    if registry.projects.is_empty() {
        println!("no project registered. run: culm project new <path>");
        return Ok(Outcome::Done);
    }
    for entry in &registry.projects {
        let project = load_or_new(store, entry)?;
        let active = project.sessions.iter().filter(|s| s.is_active()).count();
        println!(
            "{:<20} {:<50} {} repos, {} sessions ({} active)",
            entry.slug,
            entry.root.display(),
            project.repos.len(),
            project.sessions.len(),
            active
        );
    }
    Ok(Outcome::Done)
}

/// Prints what a removal touches, and names every worktree that holds uncommitted
/// work, so the confirmation is informed.
fn describe(project: &Project, plan: &Removal, git: &dyn Git) -> Result<()> {
    println!("removing project {}", project.slug);
    println!("  registry entry and culm state");
    for dir in &plan.claude_dirs {
        println!("  claude data  ~/.claude/projects/{dir}");
    }
    for (_, worktree) in &plan.worktrees {
        let dirty = git.worktree_is_dirty(worktree).unwrap_or(false);
        let mark = if dirty { "  UNCOMMITTED WORK" } else { "" };
        println!("  worktree     {}{mark}", worktree.display());
    }
    println!("no branch, no source file, and no repository is deleted.");
    Ok(())
}

fn project_rm(
    store: &dyn Store,
    git: &dyn Git,
    slug: &str,
    force: bool,
    recursive: bool,
) -> Result<Outcome> {
    let mut registry = store.load_registry()?;
    let entry = registry
        .find(slug)
        .ok_or_else(|| anyhow!("no project named {slug}. run: culm project list"))?
        .clone();
    let project = load_or_new(store, &entry)?;
    let plan = plan_removal(&project, &store.list_claude_dirs()?, recursive);

    describe(&project, &plan, git)?;
    if !force {
        println!("type the project name to confirm:");
        let mut typed = String::new();
        std::io::stdin().read_line(&mut typed)?;
        if typed.trim() != slug {
            println!("cancelled. nothing was removed.");
            return Ok(Outcome::Done);
        }
    }

    for (repo, worktree) in &plan.worktrees {
        if let Err(e) = git.worktree_remove(repo, worktree) {
            println!("could not remove {}: {e}", worktree.display());
        }
    }
    for dir in &plan.claude_dirs {
        store.remove_claude_dir(dir)?;
    }
    store.remove_project(slug)?;
    registry.projects.retain(|p| p.slug != slug);
    store.save_registry(&registry)?;
    println!("project {slug} removed");
    Ok(Outcome::Done)
}

fn repo_add(store: &dyn Store, path: &Path, slug: Option<&str>, cwd: &Path) -> Result<Outcome> {
    let mut project = resolve(store, slug, cwd)?;
    let repo_path = std::fs::canonicalize(path).map_err(|e| anyhow!("{}: {e}", path.display()))?;
    if !repo_path.join(".git").exists() {
        bail!("{} holds no .git", repo_path.display());
    }
    let name = repo_path
        .file_name()
        .map(|n| slugify(&n.to_string_lossy()))
        .ok_or_else(|| anyhow!("{} has no directory name", repo_path.display()))?;
    if let Some(existing) = project.repo(&name) {
        bail!("{} already holds {}", project.slug, existing.name);
    }
    project.repos.push(Repository {
        name: name.clone(),
        path: repo_path,
    });
    store.save_project(&project)?;
    println!("{} now holds {}", project.slug, name);
    Ok(Outcome::Done)
}

fn repo_rm(store: &dyn Store, name: &str, slug: Option<&str>, cwd: &Path) -> Result<Outcome> {
    let mut project = resolve(store, slug, cwd)?;
    let before = project.repos.len();
    project.repos.retain(|r| r.name != name);
    if project.repos.len() == before {
        bail!("{} holds no repository named {name}", project.slug);
    }
    store.save_project(&project)?;
    println!("{} no longer holds {name}", project.slug);
    Ok(Outcome::Done)
}

/// Lists the repositories of a project, and where each one is checked out.
fn repo_list(store: &dyn Store, slug: Option<&str>, cwd: &Path) -> Result<Outcome> {
    let project = resolve(store, slug, cwd)?;
    if project.repos.is_empty() {
        println!(
            "{} holds no repository. run: culm project repo add <path>",
            project.slug
        );
        return Ok(Outcome::Done);
    }
    for repo in &project.repos {
        println!("{:<20} {}", repo.name, repo.path.display());
    }
    Ok(Outcome::Done)
}

fn open(
    store: &dyn Store,
    namer: &dyn Namer,
    slug: Option<&str>,
    cwd: &Path,
    import: bool,
) -> Result<Outcome> {
    let mut project = resolve(store, slug, cwd)?;
    if import {
        report(&import_sessions(store, namer, &mut project)?);
    }
    Ok(Outcome::Open(Box::new(project)))
}

/// Finds the project by slug, or the project that owns the working directory.
fn resolve(store: &dyn Store, slug: Option<&str>, cwd: &Path) -> Result<Project> {
    let registry = store.load_registry()?;
    let entry = find_entry(&registry, slug, cwd)?;
    load_or_new(store, entry)
}

/// The named project, or the project that owns the working directory.
fn find_entry<'a>(
    registry: &'a Registry,
    slug: Option<&str>,
    cwd: &Path,
) -> Result<&'a ProjectEntry> {
    match slug {
        Some(slug) => registry
            .find(slug)
            .ok_or_else(|| anyhow!("no project named {slug}. run: culm project list")),
        None => registry.owner_of(cwd).ok_or_else(|| {
            anyhow!(
                "{} belongs to no project. run: culm project new {}",
                cwd.display(),
                cwd.display()
            )
        }),
    }
}

fn load_or_new(store: &dyn Store, entry: &ProjectEntry) -> Result<Project> {
    Ok(store
        .load_project(&entry.slug)?
        .unwrap_or_else(|| Project::new(&entry.slug, &entry.root)))
}

/// The registry, for a caller that only needs the project list.
pub fn registry(store: &dyn Store) -> Result<Registry> {
    store.load_registry()
}
