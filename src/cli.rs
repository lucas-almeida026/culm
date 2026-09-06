//! The command line. Every project operation is reachable without the interface, so
//! a shell script reaches culm too.

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};
use clap::{Parser, Subcommand};

use crate::hooks;
use crate::project::{Project, ProjectEntry, Registry, Repository, slugify};
use crate::store::Store;

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
    },
    /// List every registered project.
    List,
    /// Add or remove a repository of a project.
    #[command(subcommand)]
    Repo(RepoCmd),
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
pub fn run(cli: Cli, store: &dyn Store, exe: &Path, cwd: &Path) -> Result<Outcome> {
    match cli.command {
        None => open(store, None, cwd),
        Some(Command::Open { project }) => open(store, project.as_deref(), cwd),
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
        Some(Command::Project(ProjectCmd::New { path })) => project_new(store, &path),
        Some(Command::Project(ProjectCmd::List)) => project_list(store),
        Some(Command::Project(ProjectCmd::Repo(RepoCmd::Add { path, project }))) => {
            repo_add(store, &path, project.as_deref(), cwd)
        }
        Some(Command::Project(ProjectCmd::Repo(RepoCmd::Rm { name, project }))) => {
            repo_rm(store, &name, project.as_deref(), cwd)
        }
    }
}

fn project_new(store: &dyn Store, path: &Path) -> Result<Outcome> {
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
    let entry = registry.add(&root);
    store.save_registry(&registry)?;
    store.save_project(&Project::new(&entry.slug, &root))?;
    println!("project {} at {}", entry.slug, root.display());
    Ok(Outcome::Done)
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

fn open(store: &dyn Store, slug: Option<&str>, cwd: &Path) -> Result<Outcome> {
    Ok(Outcome::Open(Box::new(resolve(store, slug, cwd)?)))
}

/// Finds the project by slug, or the project that owns the working directory.
fn resolve(store: &dyn Store, slug: Option<&str>, cwd: &Path) -> Result<Project> {
    let registry = store.load_registry()?;
    let entry = match slug {
        Some(slug) => registry
            .find(slug)
            .ok_or_else(|| anyhow!("no project named {slug}. run: culm project list"))?,
        None => registry.owner_of(cwd).ok_or_else(|| {
            anyhow!(
                "{} belongs to no project. run: culm project new {}",
                cwd.display(),
                cwd.display()
            )
        })?,
    };
    load_or_new(store, entry)
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
