//! The command line. Every project operation is reachable without the interface, so
//! a shell script reaches culm too.

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};
use clap::{Parser, Subcommand};

use crate::command::{Command as SessionCommand, Reply};
use crate::config::SetError;
use crate::git::Git;
use crate::hooks;
use crate::namer::Namer;
use crate::project::{
    Project, ProjectEntry, Registry, Removal, Repository, SessionRecord, SessionRepo, SessionState,
    dir_belongs_to, plan_removal, slugify, transcript_dir_name, unique_slug,
};
use crate::store::Store;
use crate::transcript;

/// The environment variable that carries a session's id into its process, so
/// `culm session` run inside the session knows which session it is.
pub const SESSION_VAR: &str = "CULM_SESSION";
/// The environment variable that carries the project slug into a session.
pub const PROJECT_VAR: &str = "CULM_PROJECT";

/// Who runs the command line: the session and project a culm session hands down
/// through `SESSION_VAR` and `PROJECT_VAR`. Empty outside a session.
///
/// `main` reads the environment. The library never does, so a test that runs inside
/// a culm session sees the same result as one that does not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Caller {
    pub session: Option<String>,
    pub project: Option<String>,
}

impl Caller {
    /// Reads the caller from the process environment.
    #[must_use]
    pub fn from_env() -> Self {
        let read = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        Self {
            session: read(SESSION_VAR),
            project: read(PROJECT_VAR),
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "culm",
    version,
    about = "Many projects as tabs, and many Claude Code sessions in each."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Open projects in the interface, one tab each.
    Open {
        /// Project slugs, at most eight. Defaults to the project that owns the
        /// working directory.
        projects: Vec<String>,
        /// Open the projects of a workspace instead.
        #[arg(short = 'w', long, conflicts_with = "projects")]
        workspace: Option<String>,
        /// Import every Claude Code session found under each root before opening.
        #[arg(long)]
        import_native_sessions: bool,
    },
    /// Name sets of projects, so one command opens all of them as tabs.
    #[command(subcommand)]
    Workspace(WorkspaceCmd),
    /// Register projects and their repositories.
    #[command(subcommand)]
    Project(ProjectCmd),
    /// Install or remove the Claude Code hook entries that feed attention markers.
    #[command(subcommand)]
    Hooks(HooksCmd),
    /// Read or change the settings that apply to every project.
    #[command(subcommand)]
    Config(ConfigCmd),
    /// Read or change what a session registered: its ticket, its port number, and
    /// the worktrees it works through. Inside a culm session, every subcommand acts
    /// on that session unless --session names another.
    #[command(subcommand)]
    Session(SessionCmd),
    /// See which port numbers the sessions of a project hold.
    #[command(subcommand)]
    Port(PortCmd),
    /// Hook client. Reads one payload on stdin and posts it to the interface.
    #[command(hide = true)]
    Hook,
}

#[derive(Debug, Subcommand)]
pub enum WorkspaceCmd {
    /// Create a workspace, with the projects it opens, in tab order.
    New { name: String, projects: Vec<String> },
    /// List every workspace and its projects.
    List,
    /// Show the projects of one workspace, in tab order.
    Show { name: String },
    /// Delete a workspace. Its projects are left alone.
    Rm { name: String },
    /// Add a project to a workspace, as its last tab.
    Add { name: String, project: String },
    /// Remove a project from a workspace. The project itself is left alone.
    Remove { name: String, project: String },
    /// Rename a workspace.
    Rename { name: String, new_name: String },
}

#[derive(Debug, Subcommand)]
pub enum ConfigCmd {
    /// Show every setting and its value.
    List,
    /// Change one setting.
    Set {
        /// Setting name, as `config list` prints it.
        key: String,
        /// The new value. A switch takes true or false.
        value: String,
    },
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
        /// Also remove the Claude Code data of every path under the root.
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

/// The session and project a `culm session` command acts on.
#[derive(Debug, clap::Args)]
pub struct Target {
    /// The session, by id, slug, or name. Defaults to the session the command runs
    /// in.
    #[arg(long)]
    pub session: Option<String>,
    /// The project. Defaults to the project of the session the command runs in, then
    /// to the project that owns the working directory.
    #[arg(long)]
    pub project: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum SessionCmd {
    /// List every session that is not archived, with its ticket, port number, and
    /// worktrees.
    List {
        #[arg(long)]
        project: Option<String>,
    },
    /// Show what one session registered.
    Show {
        #[command(flatten)]
        target: Target,
    },
    /// Set the ticket the session works on.
    Ticket {
        /// The ticket, such as CLUB-1138.
        #[arg(required_unless_present = "clear")]
        ticket: Option<String>,
        /// Remove the ticket.
        #[arg(long, conflicts_with = "ticket")]
        clear: bool,
        #[command(flatten)]
        target: Target,
    },
    /// Set the port number of the session, from 0 to 9. Another session may hold the
    /// same number. See the free ones with: culm port list
    Port {
        #[arg(required_unless_present = "clear")]
        port: Option<u8>,
        /// Remove the port number.
        #[arg(long, conflicts_with = "port")]
        clear: bool,
        #[command(flatten)]
        target: Target,
    },
    /// Register or forget a worktree the session works through.
    #[command(subcommand)]
    Worktree(WorktreeCmd),
    /// Find the worktrees each session worked in, from its transcript, and offer to
    /// register them.
    Adopt {
        #[arg(long)]
        project: Option<String>,
        /// Register every worktree found, without asking.
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum WorktreeCmd {
    /// Register a worktree. Any path inside it works. culm only records it, and
    /// never creates or removes a worktree.
    Add {
        path: PathBuf,
        #[command(flatten)]
        target: Target,
    },
    /// Forget a worktree. It stays on disk.
    Rm {
        path: PathBuf,
        #[command(flatten)]
        target: Target,
    },
}

#[derive(Debug, Subcommand)]
pub enum PortCmd {
    /// Each port number from 0 to 9, and the sessions that hold it.
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

/// The projects an `open` hands to the interface, in tab order.
#[derive(Debug)]
pub struct OpenPlan {
    pub projects: Vec<Project>,
    /// The workspace the projects came from, and the projects it held.
    pub workspace: Option<crate::browser::WorkspaceOrigin>,
}

/// What the caller must do after the command ran.
#[derive(Debug)]
pub enum Outcome {
    /// Open these projects in the interface.
    Open(Box<OpenPlan>),
    /// The command is finished.
    Done,
}

/// Runs one command. Printing happens here, because a command line is output.
#[allow(clippy::too_many_arguments)]
pub fn run(
    cli: Cli,
    store: &dyn Store,
    git: &dyn Git,
    namer: &dyn Namer,
    hooks: &dyn crate::hooks::HookListener,
    exe: &Path,
    cwd: &Path,
    caller: &Caller,
) -> Result<Outcome> {
    match cli.command {
        None => open(store, namer, &[], cwd, false),
        Some(Command::Open {
            projects,
            workspace: Some(workspace),
            import_native_sessions,
        }) if projects.is_empty() => {
            open_workspace(store, namer, &workspace, import_native_sessions)
        }
        Some(Command::Open {
            projects,
            import_native_sessions,
            ..
        }) => open(store, namer, &projects, cwd, import_native_sessions),
        Some(Command::Workspace(cmd)) => workspace_cmd(store, cmd),
        Some(Command::Config(ConfigCmd::List)) => config_list(store),
        Some(Command::Config(ConfigCmd::Set { key, value })) => config_set(store, &key, &value),
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
            project_rename(store, hooks, &name, project.as_deref(), cwd)
        }
        Some(Command::Project(ProjectCmd::List)) => project_list(store),
        Some(Command::Project(ProjectCmd::Import { project })) => {
            project_import_cmd(store, namer, project.as_deref(), cwd)
        }
        Some(Command::Project(ProjectCmd::Rm {
            project,
            force,
            recursive,
        })) => project_rm(store, &project, force, recursive),
        Some(Command::Project(ProjectCmd::Repo(RepoCmd::Add { path, project }))) => {
            repo_add(store, &path, project.as_deref(), cwd)
        }
        Some(Command::Project(ProjectCmd::Repo(RepoCmd::Rm { name, project }))) => {
            repo_rm(store, &name, project.as_deref(), cwd)
        }
        Some(Command::Project(ProjectCmd::Repo(RepoCmd::List { project }))) => {
            repo_list(store, project.as_deref(), cwd)
        }
        Some(Command::Session(cmd)) => {
            let ctx = SessionCtx {
                store,
                git,
                hooks,
                cwd,
                caller,
            };
            session_cmd(&ctx, cmd)
        }
        Some(Command::Port(PortCmd::List { project })) => {
            let ctx = SessionCtx {
                store,
                git,
                hooks,
                cwd,
                caller,
            };
            let slug = ctx.project(project.as_deref())?.slug;
            print_reply(&deliver(&ctx, &slug, &SessionCommand::Ports)?)
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
///
/// A project open in a window is refused. The window writes its state under the
/// name it opened with, so every change after the rename would land in a file no
/// name points at.
fn project_rename(
    store: &dyn Store,
    hooks: &dyn crate::hooks::HookListener,
    new_name: &str,
    slug: Option<&str>,
    cwd: &Path,
) -> Result<Outcome> {
    let mut registry = store.load_registry()?;
    let entry = find_entry(&registry, slug, cwd)?.clone();
    if hooks.is_open(&entry.slug) {
        bail!(
            "{} is open in culm. close its tab, then rename.",
            entry.slug
        );
    }
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
    let mut workspaces = store.load_workspaces()?;
    workspaces.rename_project(&entry.slug, &new_slug);
    store.save_workspaces(&workspaces)?;
    println!("project {} is now {new_slug}", entry.slug);
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
                ticket: None,
                port: None,
                // The transcript exists, so the session has run and resumes.
                started: true,
                last_active: file.modified_secs,
                last_input: 0,
                recap: None,
                recap_at: 0,
                archived_from: None,
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

/// Prints what a removal touches, so the confirmation is informed.
fn describe(project: &Project, plan: &Removal) {
    println!("removing project {}", project.slug);
    println!("  registry entry and culm state");
    for dir in &plan.claude_dirs {
        println!("  claude data  ~/.claude/projects/{dir}");
    }
    println!("no worktree, no branch, no source file, and no repository is deleted.");
}

fn project_rm(store: &dyn Store, slug: &str, force: bool, recursive: bool) -> Result<Outcome> {
    let mut registry = store.load_registry()?;
    let entry = registry
        .find(slug)
        .ok_or_else(|| anyhow!("no project named {slug}. run: culm project list"))?
        .clone();
    let project = load_or_new(store, &entry)?;
    let plan = plan_removal(&project, &store.list_claude_dirs()?, recursive);

    describe(&project, &plan);
    if !force {
        println!("type the project name to confirm:");
        let mut typed = String::new();
        std::io::stdin().read_line(&mut typed)?;
        if typed.trim() != slug {
            println!("cancelled. nothing was removed.");
            return Ok(Outcome::Done);
        }
    }

    for dir in &plan.claude_dirs {
        store.remove_claude_dir(dir)?;
    }
    store.remove_project(slug)?;
    registry.projects.retain(|p| p.slug != slug);
    store.save_registry(&registry)?;
    let mut workspaces = store.load_workspaces()?;
    workspaces.drop_project(slug);
    store.save_workspaces(&workspaces)?;
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
fn config_list(store: &dyn Store) -> Result<Outcome> {
    for (key, value) in store.load_config()?.settings() {
        println!("{key:<20} {value}");
    }
    Ok(Outcome::Done)
}

fn config_set(store: &dyn Store, key: &str, value: &str) -> Result<Outcome> {
    let mut config = store.load_config()?;
    match config.set(key, value) {
        Ok(()) => {
            store.save_config(&config)?;
            println!("{key} is now {value}");
            Ok(Outcome::Done)
        }
        // The two mistakes are different, so they are reported differently.
        Err(SetError::Key) => {
            bail!("no setting is called {key}. run: culm config list")
        }
        Err(SetError::Value) => {
            bail!("{key} does not take {value}. it takes true or false")
        }
    }
}

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

/// Opens the named projects, one tab each, or the project that owns the working
/// directory when none is named. A project named twice opens once.
fn open(
    store: &dyn Store,
    namer: &dyn Namer,
    slugs: &[String],
    cwd: &Path,
    import: bool,
) -> Result<Outcome> {
    let slugs = crate::workspace::without_repeats(slugs);
    if slugs.len() > crate::workspace::MAX_PROJECTS {
        bail!(
            "culm opens at most {} projects at once",
            crate::workspace::MAX_PROJECTS
        );
    }
    let mut projects = Vec::new();
    if slugs.is_empty() {
        projects.push(resolve(store, None, cwd)?);
    }
    for slug in &slugs {
        projects.push(resolve(store, Some(slug), cwd)?);
    }
    if import {
        for project in &mut projects {
            report(&import_sessions(store, namer, project)?);
        }
    }
    Ok(Outcome::Open(Box::new(OpenPlan {
        projects,
        workspace: None,
    })))
}

/// Opens the projects of a workspace, in its order. A project removed since the
/// workspace was saved is reported and skipped.
fn open_workspace(
    store: &dyn Store,
    namer: &dyn Namer,
    name: &str,
    import: bool,
) -> Result<Outcome> {
    let workspaces = store.load_workspaces()?;
    let workspace = workspaces
        .find(name)
        .ok_or_else(|| anyhow!("{}", crate::workspace::WorkspaceError::Unknown(name.into())))?
        .clone();
    let registry = store.load_registry()?;
    let mut projects = Vec::new();
    for slug in &workspace.projects {
        match registry.find(slug) {
            Some(entry) => projects.push(load_or_new(store, entry)?),
            None => println!(
                "{} holds {slug}, which is no longer a project. skipped.",
                workspace.name
            ),
        }
    }
    if projects.is_empty() {
        bail!(
            "{} holds no project. add one with: culm workspace add {} <project>",
            workspace.name,
            workspace.name
        );
    }
    if import {
        for project in &mut projects {
            report(&import_sessions(store, namer, project)?);
        }
    }
    Ok(Outcome::Open(Box::new(OpenPlan {
        projects,
        workspace: Some(crate::browser::WorkspaceOrigin {
            name: workspace.name,
            projects: workspace.projects,
        }),
    })))
}

fn workspace_cmd(store: &dyn Store, cmd: WorkspaceCmd) -> Result<Outcome> {
    let mut all = store.load_workspaces()?;
    let registry = store.load_registry()?;
    let known = |slug: &str| -> Result<()> {
        if registry.find(slug).is_none() {
            bail!("no project named {slug}. run: culm project list");
        }
        Ok(())
    };
    match cmd {
        WorkspaceCmd::List => {
            if all.workspaces.is_empty() {
                println!("no workspace yet. run: culm workspace new <name> <project>...");
            }
            for workspace in &all.workspaces {
                println!("{:<20} {}", workspace.name, workspace.projects.join(" "));
            }
            return Ok(Outcome::Done);
        }
        WorkspaceCmd::Show { name } => {
            let workspace = all
                .find(&name)
                .ok_or_else(|| anyhow!("{}", crate::workspace::WorkspaceError::Unknown(name)))?;
            for project in &workspace.projects {
                println!("{project}");
            }
            return Ok(Outcome::Done);
        }
        WorkspaceCmd::New { name, projects } => {
            for project in &projects {
                known(project)?;
            }
            let slug = all.create(&name, &projects).map_err(|e| anyhow!("{e}"))?;
            println!("workspace {slug} created. open it with: culm open -w {slug}");
        }
        WorkspaceCmd::Rm { name } => {
            let gone = all.remove(&name).map_err(|e| anyhow!("{e}"))?;
            println!(
                "workspace {} removed. its projects are untouched.",
                gone.name
            );
        }
        WorkspaceCmd::Add { name, project } => {
            known(&project)?;
            all.add_project(&name, &project)
                .map_err(|e| anyhow!("{e}"))?;
            println!("{name} now holds {project}");
        }
        WorkspaceCmd::Remove { name, project } => {
            all.remove_project(&name, &project)
                .map_err(|e| anyhow!("{e}"))?;
            println!("{name} no longer holds {project}");
        }
        WorkspaceCmd::Rename { name, new_name } => {
            let slug = all.rename(&name, &new_name).map_err(|e| anyhow!("{e}"))?;
            println!("workspace {name} is now {slug}");
        }
    }
    store.save_workspaces(&all)?;
    Ok(Outcome::Done)
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

/// What every `culm session` command needs.
struct SessionCtx<'a> {
    store: &'a dyn Store,
    git: &'a dyn Git,
    hooks: &'a dyn crate::hooks::HookListener,
    cwd: &'a Path,
    caller: &'a Caller,
}

impl SessionCtx<'_> {
    /// The project named by `--project`, then the caller's, then the one that owns
    /// the working directory.
    fn project(&self, named: Option<&str>) -> Result<ProjectEntry> {
        let registry = self.store.load_registry()?;
        let slug = named.or(self.caller.project.as_deref());
        Ok(find_entry(&registry, slug, self.cwd)?.clone())
    }

    /// The project and the session a command acts on. The caller's session counts
    /// only inside the caller's own project.
    fn target(&self, target: &Target) -> Result<(ProjectEntry, String)> {
        let entry = self.project(target.project.as_deref())?;
        let inherited = self
            .caller
            .session
            .clone()
            .filter(|_| self.caller.project.as_deref() == Some(entry.slug.as_str()));
        let session = target.session.clone().or(inherited).ok_or_else(|| {
            anyhow!("name the session with --session, or run this inside a culm session")
        })?;
        Ok((entry, session))
    }

    /// An absolute path, read against the working directory of the command line.
    fn absolute(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.cwd.join(path)
        }
    }
}

/// Applies one command to a project. A project open in a window gets the command
/// through its socket, because the window owns its state and would write over a
/// change made to the file. A closed project is changed in its file.
fn deliver(ctx: &SessionCtx<'_>, slug: &str, command: &SessionCommand) -> Result<Reply> {
    if ctx.hooks.is_open(slug) {
        return ctx.hooks.request(slug, command);
    }
    let registry = ctx.store.load_registry()?;
    let entry = registry
        .find(slug)
        .ok_or_else(|| anyhow!("no project named {slug}. run: culm project list"))?;
    let mut project = load_or_new(ctx.store, entry)?;
    let mut records: Vec<&mut SessionRecord> = project.sessions.iter_mut().collect();
    let reply = crate::command::apply(&mut records, command);
    if reply.changed {
        ctx.store.save_project(&project)?;
    }
    Ok(reply)
}

/// Prints a reply. A refusal is an error, so a script sees a failing exit code.
fn print_reply(reply: &Reply) -> Result<Outcome> {
    if !reply.ok {
        bail!("{}", reply.text.trim_end());
    }
    println!("{}", reply.text.trim_end());
    Ok(Outcome::Done)
}

/// What git says about the checkout that holds `path`, as a worktree to register.
fn worktree_at(ctx: &SessionCtx<'_>, path: &Path) -> Result<SessionRepo> {
    let path = ctx.absolute(path);
    let info = ctx
        .git
        .worktree_info(&path)?
        .ok_or_else(|| anyhow!("{} is in no git repository", path.display()))?;
    Ok(SessionRepo {
        name: info.repo,
        worktree: info.top,
        branch: info.branch,
    })
}

fn session_cmd(ctx: &SessionCtx<'_>, cmd: SessionCmd) -> Result<Outcome> {
    let (slug, command) = match cmd {
        SessionCmd::List { project } => {
            (ctx.project(project.as_deref())?.slug, SessionCommand::List)
        }
        SessionCmd::Show { target } => {
            let (entry, session) = ctx.target(&target)?;
            (entry.slug, SessionCommand::Show { session })
        }
        SessionCmd::Ticket {
            ticket,
            clear,
            target,
        } => {
            let (entry, session) = ctx.target(&target)?;
            let ticket = if clear { None } else { ticket };
            (entry.slug, SessionCommand::Ticket { session, ticket })
        }
        SessionCmd::Port {
            port,
            clear,
            target,
        } => {
            let (entry, session) = ctx.target(&target)?;
            let port = if clear { None } else { port };
            (entry.slug, SessionCommand::Port { session, port })
        }
        SessionCmd::Worktree(WorktreeCmd::Add { path, target }) => {
            let (entry, session) = ctx.target(&target)?;
            let worktree = worktree_at(ctx, &path)?;
            (
                entry.slug,
                SessionCommand::WorktreeAdd { session, worktree },
            )
        }
        SessionCmd::Worktree(WorktreeCmd::Rm { path, target }) => {
            let (entry, session) = ctx.target(&target)?;
            // A worktree already deleted from disk is still forgotten by its path.
            let path = match ctx.git.worktree_info(&ctx.absolute(&path))? {
                Some(info) => info.top,
                None => ctx.absolute(&path),
            };
            (entry.slug, SessionCommand::WorktreeRm { session, path })
        }
        SessionCmd::Adopt { project, yes } => return adopt(ctx, project.as_deref(), yes),
    };
    print_reply(&deliver(ctx, &slug, &command)?)
}

/// One worktree a session worked in and has not registered, and how many lines of
/// its transcript point there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub worktree: SessionRepo,
    pub evidence: usize,
}

/// The worktrees each session worked in, from its transcript, most evidence first.
///
/// A line counts when the session's shell stood in the worktree or the session wrote
/// a file there. Only linked worktrees count, because a main checkout is shared by
/// every session. A worktree under `temp` is a scratch copy, and a worktree the
/// session already registered is left out. Archived sessions are skipped.
pub fn adoption(
    store: &dyn Store,
    git: &dyn Git,
    project: &Project,
    temp: &Path,
) -> Result<Vec<(SessionRecord, Vec<Candidate>)>> {
    let mut checkouts: std::collections::HashMap<PathBuf, Option<crate::git::WorktreeInfo>> =
        std::collections::HashMap::new();
    let mut out = Vec::new();
    for record in project.sessions.iter().filter(|r| !r.is_archived()) {
        let Ok(text) = store.read_transcript(&transcript_dir_name(&record.cwd), &record.id) else {
            continue;
        };
        let mut places = transcript::visited_dirs(&text);
        for file in transcript::edited_files(&text) {
            if let Some(dir) = file.parent() {
                places.push((dir.to_path_buf(), 1));
            }
        }
        let mut found: Vec<Candidate> = Vec::new();
        for (dir, count) in places {
            let info = match checkouts.get(&dir) {
                Some(info) => info.clone(),
                None => {
                    let info = git.worktree_info(&dir)?;
                    checkouts.insert(dir.clone(), info.clone());
                    info
                }
            };
            let Some(info) = info.filter(|i| i.linked && !i.top.starts_with(temp)) else {
                continue;
            };
            if record.repos.iter().any(|r| r.worktree == info.top) {
                continue;
            }
            match found.iter_mut().find(|c| c.worktree.worktree == info.top) {
                Some(c) => c.evidence += count,
                None => found.push(Candidate {
                    worktree: SessionRepo {
                        name: info.repo,
                        worktree: info.top,
                        branch: info.branch,
                    },
                    evidence: count,
                }),
            }
        }
        if !found.is_empty() {
            found.sort_by(|a, b| b.evidence.cmp(&a.evidence));
            out.push((record.clone(), found));
        }
    }
    Ok(out)
}

/// Offers each session the worktrees it worked in, and registers the ones the user
/// picks, or all of them with `--yes`.
fn adopt(ctx: &SessionCtx<'_>, named: Option<&str>, yes: bool) -> Result<Outcome> {
    let entry = ctx.project(named)?;
    let project = load_or_new(ctx.store, &entry)?;
    let found = adoption(ctx.store, ctx.git, &project, &std::env::temp_dir())?;
    if found.is_empty() {
        println!("no session worked in a worktree it has not registered");
        return Ok(Outcome::Done);
    }
    let sessions = match found.len() {
        1 => "1 session worked in worktrees it has".to_string(),
        n => format!("{n} sessions worked in worktrees they have"),
    };
    println!(
        "{sessions} not registered. for each session, pick the worktrees that belong to it. \
         a worktree the session only visited usually has few transcript lines.\n"
    );
    for (record, candidates) in found {
        print!("{}", offer(&record, &candidates));
        // The question ends without a newline, and stdout holds a partial line back.
        std::io::Write::flush(&mut std::io::stdout())?;
        let picked: Vec<&Candidate> = if yes {
            candidates.iter().collect()
        } else {
            let mut typed = String::new();
            std::io::stdin().read_line(&mut typed)?;
            pick(&typed, &candidates)
        };
        if picked.is_empty() {
            println!("  nothing registered for {}", record.name);
        }
        for c in picked {
            let reply = deliver(
                ctx,
                &entry.slug,
                &SessionCommand::WorktreeAdd {
                    session: record.id.clone(),
                    worktree: c.worktree.clone(),
                },
            )?;
            println!("  {}", reply.text.trim_end());
        }
    }
    Ok(Outcome::Done)
}

/// The question `adopt` asks about one session: the session, each worktree with its
/// repository and its evidence, and how to answer.
fn offer(record: &SessionRecord, candidates: &[Candidate]) -> String {
    let state = match record.state {
        SessionState::Active => "active",
        SessionState::Paused => "paused",
        SessionState::Archived => "archived",
    };
    let mut text = format!("{} ({state})\n", record.name);
    for (i, c) in candidates.iter().enumerate() {
        let lines = if c.evidence == 1 { "line" } else { "lines" };
        text.push_str(&format!(
            "  {}) {}\n     repository {}, seen on {} transcript {lines}\n",
            i + 1,
            c.worktree.worktree.display(),
            c.worktree.name,
            c.evidence
        ));
    }
    text.push_str(&format!(
        "register these worktrees for {}?\n  a = all of them, 1 3 = only those numbers, \
         Enter = none: ",
        record.name
    ));
    text
}

/// The candidates an answer picks: `a` for all, or 1-based numbers. Anything else
/// picks nothing.
fn pick<'a>(typed: &str, candidates: &'a [Candidate]) -> Vec<&'a Candidate> {
    let typed = typed.trim();
    if typed.eq_ignore_ascii_case("a") {
        return candidates.iter().collect();
    }
    typed
        .split_whitespace()
        .filter_map(|n| n.parse::<usize>().ok())
        .filter_map(|n| n.checked_sub(1).and_then(|i| candidates.get(i)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(path: &str) -> Candidate {
        Candidate {
            worktree: SessionRepo {
                name: "api".into(),
                worktree: PathBuf::from(path),
                branch: String::new(),
            },
            evidence: 1,
        }
    }

    #[test]
    fn the_adopt_question_names_the_session_its_state_and_every_way_to_answer() {
        let record = SessionRecord {
            id: "id".into(),
            name: "workspace".into(),
            slug: "workspace".into(),
            state: SessionState::Paused,
            cwd: PathBuf::from("/home/x/spm"),
            repos: Vec::new(),
            ticket: None,
            port: None,
            started: true,
            last_active: 0,
            last_input: 0,
            recap: None,
            recap_at: 0,
            archived_from: None,
        };
        let text = offer(&record, &[candidate("/w/api")]);
        assert!(text.starts_with("workspace (paused)\n"), "{text}");
        assert!(text.contains("1) /w/api"), "{text}");
        assert!(text.contains("seen on 1 transcript line\n"), "{text}");
        assert!(
            text.contains("register these worktrees for workspace?"),
            "{text}"
        );
        assert!(
            text.contains("a = all") && text.contains("1 3 =") && text.contains("Enter = none"),
            "{text}"
        );
    }

    #[test]
    fn an_answer_picks_all_some_or_none() {
        let all = [candidate("/a"), candidate("/b"), candidate("/c")];
        assert_eq!(pick("a\n", &all).len(), 3);
        let some: Vec<&Path> = pick("3 1", &all)
            .iter()
            .map(|c| c.worktree.worktree.as_path())
            .collect();
        assert_eq!(some, vec![Path::new("/c"), Path::new("/a")]);
        assert!(pick("\n", &all).is_empty());
        assert!(pick("0 9 x", &all).is_empty(), "out of range picks nothing");
    }
}
