//! Workspaces. A workspace names an ordered set of projects, so one command opens
//! all of them as tabs. Pure values and pure functions, so every rule here is unit
//! tested.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::project::slugify;

/// The most projects one window holds, and so the most one workspace holds. One
/// digit from 1 to 8 addresses each tab.
pub const MAX_PROJECTS: usize = 8;

/// One named set of projects, in tab order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    pub name: String,
    /// Project slugs, in the order the tabs open.
    #[serde(default)]
    pub projects: Vec<String>,
}

/// Every workspace culm knows, in the order they were created.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspaces {
    #[serde(default)]
    pub workspaces: Vec<Workspace>,
}

/// Why a change to the workspaces was refused. The caller reports it by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceError {
    /// The name holds no letter and no digit, so it gives no slug.
    EmptyName,
    Taken(String),
    Unknown(String),
    /// The workspace already holds `MAX_PROJECTS` projects.
    Full(String),
    AlreadyHolds {
        workspace: String,
        project: String,
    },
    DoesNotHold {
        workspace: String,
        project: String,
    },
}

impl fmt::Display for WorkspaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyName => write!(f, "a workspace needs a name with a letter or a digit"),
            Self::Taken(name) => write!(f, "a workspace named {name} already exists"),
            Self::Unknown(name) => {
                write!(f, "no workspace named {name}. run: culm workspace list")
            }
            Self::Full(name) => write!(f, "{name} already holds {MAX_PROJECTS} projects"),
            Self::AlreadyHolds { workspace, project } => {
                write!(f, "{workspace} already holds {project}")
            }
            Self::DoesNotHold { workspace, project } => {
                write!(f, "{workspace} does not hold {project}")
            }
        }
    }
}

impl std::error::Error for WorkspaceError {}

/// The slug a workspace name becomes, or `None` when the name gives none.
#[must_use]
pub fn workspace_slug(name: &str) -> Option<String> {
    name.chars()
        .any(|c| c.is_ascii_alphanumeric())
        .then(|| slugify(name))
}

/// The projects with every repeat removed. The first place a project appears wins.
#[must_use]
pub fn without_repeats(projects: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for project in projects {
        if !out.contains(project) {
            out.push(project.clone());
        }
    }
    out
}

impl Workspaces {
    #[must_use]
    pub fn find(&self, name: &str) -> Option<&Workspace> {
        let slug = workspace_slug(name)?;
        self.workspaces.iter().find(|w| w.name == slug)
    }

    fn find_mut(&mut self, name: &str) -> Result<&mut Workspace, WorkspaceError> {
        let slug = workspace_slug(name).ok_or(WorkspaceError::EmptyName)?;
        self.workspaces
            .iter_mut()
            .find(|w| w.name == slug)
            .ok_or(WorkspaceError::Unknown(slug))
    }

    /// Creates a workspace and returns the slug it was stored under.
    pub fn create(&mut self, name: &str, projects: &[String]) -> Result<String, WorkspaceError> {
        let slug = workspace_slug(name).ok_or(WorkspaceError::EmptyName)?;
        if self.workspaces.iter().any(|w| w.name == slug) {
            return Err(WorkspaceError::Taken(slug));
        }
        let projects = without_repeats(projects);
        if projects.len() > MAX_PROJECTS {
            return Err(WorkspaceError::Full(slug));
        }
        self.workspaces.push(Workspace {
            name: slug.clone(),
            projects,
        });
        Ok(slug)
    }

    /// Replaces the projects of a workspace, as a save from the interface does.
    pub fn set_projects(&mut self, name: &str, projects: &[String]) -> Result<(), WorkspaceError> {
        let workspace = self.find_mut(name)?;
        let projects = without_repeats(projects);
        if projects.len() > MAX_PROJECTS {
            return Err(WorkspaceError::Full(workspace.name.clone()));
        }
        workspace.projects = projects;
        Ok(())
    }

    /// Forgets a workspace. Its projects are left alone.
    pub fn remove(&mut self, name: &str) -> Result<Workspace, WorkspaceError> {
        let slug = workspace_slug(name).ok_or(WorkspaceError::EmptyName)?;
        let at = self
            .workspaces
            .iter()
            .position(|w| w.name == slug)
            .ok_or(WorkspaceError::Unknown(slug))?;
        Ok(self.workspaces.remove(at))
    }

    /// Renames a workspace and returns the new slug.
    pub fn rename(&mut self, name: &str, new_name: &str) -> Result<String, WorkspaceError> {
        let new_slug = workspace_slug(new_name).ok_or(WorkspaceError::EmptyName)?;
        let old = workspace_slug(name).ok_or(WorkspaceError::EmptyName)?;
        if new_slug != old && self.workspaces.iter().any(|w| w.name == new_slug) {
            return Err(WorkspaceError::Taken(new_slug));
        }
        self.find_mut(name)?.name.clone_from(&new_slug);
        Ok(new_slug)
    }

    /// Appends a project to a workspace, as the last tab.
    pub fn add_project(&mut self, name: &str, project: &str) -> Result<(), WorkspaceError> {
        let workspace = self.find_mut(name)?;
        if workspace.projects.iter().any(|p| p == project) {
            return Err(WorkspaceError::AlreadyHolds {
                workspace: workspace.name.clone(),
                project: project.to_string(),
            });
        }
        if workspace.projects.len() >= MAX_PROJECTS {
            return Err(WorkspaceError::Full(workspace.name.clone()));
        }
        workspace.projects.push(project.to_string());
        Ok(())
    }

    pub fn remove_project(&mut self, name: &str, project: &str) -> Result<(), WorkspaceError> {
        let workspace = self.find_mut(name)?;
        let Some(at) = workspace.projects.iter().position(|p| p == project) else {
            return Err(WorkspaceError::DoesNotHold {
                workspace: workspace.name.clone(),
                project: project.to_string(),
            });
        };
        workspace.projects.remove(at);
        Ok(())
    }

    /// Follows a project rename into every workspace that holds the project.
    pub fn rename_project(&mut self, old: &str, new: &str) {
        for workspace in &mut self.workspaces {
            for project in &mut workspace.projects {
                if project == old {
                    *project = new.to_string();
                }
            }
        }
    }

    /// Takes a removed project out of every workspace that held it.
    pub fn drop_project(&mut self, slug: &str) {
        for workspace in &mut self.workspaces {
            workspace.projects.retain(|p| p != slug);
        }
    }
}

#[cfg(test)]
mod tests {
    // Test code may use `expect` with a message. Library code may not.
    #![allow(clippy::expect_used)]

    use super::*;

    fn names(projects: &[&str]) -> Vec<String> {
        projects.iter().map(|p| (*p).to_string()).collect()
    }

    #[test]
    fn a_workspace_is_stored_under_its_slug() {
        let mut all = Workspaces::default();
        let slug = all
            .create("Billing Rewrite", &names(&["api", "web"]))
            .expect("the workspace is created");
        assert_eq!(slug, "billing-rewrite");
        assert_eq!(
            all.find("billing rewrite").map(|w| w.projects.clone()),
            Some(names(&["api", "web"]))
        );
    }

    #[test]
    fn a_name_with_no_letter_or_digit_is_refused() {
        let mut all = Workspaces::default();
        assert_eq!(all.create("  !! ", &[]), Err(WorkspaceError::EmptyName));
        assert_eq!(all.create("", &[]), Err(WorkspaceError::EmptyName));
    }

    #[test]
    fn a_taken_name_is_refused() {
        let mut all = Workspaces::default();
        all.create("work", &[]).expect("the first is created");
        assert_eq!(
            all.create("Work", &[]),
            Err(WorkspaceError::Taken("work".into()))
        );
    }

    #[test]
    fn a_repeated_project_is_kept_once_in_its_first_place() {
        let mut all = Workspaces::default();
        all.create("w", &names(&["a", "b", "a"]))
            .expect("the workspace is created");
        assert_eq!(
            all.find("w").map(|w| w.projects.clone()),
            Some(names(&["a", "b"]))
        );
    }

    #[test]
    fn a_ninth_project_is_refused() {
        let mut all = Workspaces::default();
        let eight = names(&["a", "b", "c", "d", "e", "f", "g", "h"]);
        all.create("w", &eight).expect("eight fit");
        assert_eq!(
            all.add_project("w", "i"),
            Err(WorkspaceError::Full("w".into()))
        );
        let nine = names(&["a", "b", "c", "d", "e", "f", "g", "h", "i"]);
        assert_eq!(
            all.create("x", &nine),
            Err(WorkspaceError::Full("x".into()))
        );
    }

    #[test]
    fn a_project_is_added_as_the_last_tab_and_removed_by_name() {
        let mut all = Workspaces::default();
        all.create("w", &names(&["a"])).expect("created");
        all.add_project("w", "b").expect("b is added");
        assert_eq!(
            all.add_project("w", "b"),
            Err(WorkspaceError::AlreadyHolds {
                workspace: "w".into(),
                project: "b".into()
            })
        );
        all.remove_project("w", "a").expect("a is removed");
        assert_eq!(
            all.find("w").map(|w| w.projects.clone()),
            Some(names(&["b"]))
        );
        assert_eq!(
            all.remove_project("w", "a"),
            Err(WorkspaceError::DoesNotHold {
                workspace: "w".into(),
                project: "a".into()
            })
        );
    }

    #[test]
    fn a_rename_keeps_the_projects_and_refuses_a_taken_name() {
        let mut all = Workspaces::default();
        all.create("one", &names(&["a"])).expect("created");
        all.create("two", &[]).expect("created");
        assert_eq!(
            all.rename("one", "two"),
            Err(WorkspaceError::Taken("two".into()))
        );
        assert_eq!(all.rename("one", "First One"), Ok("first-one".into()));
        assert_eq!(
            all.find("first-one").map(|w| w.projects.clone()),
            Some(names(&["a"]))
        );
        assert!(all.find("one").is_none());
    }

    #[test]
    fn an_unknown_workspace_is_named_in_the_refusal() {
        let mut all = Workspaces::default();
        assert_eq!(
            all.remove("ghost"),
            Err(WorkspaceError::Unknown("ghost".into()))
        );
    }

    #[test]
    fn a_project_rename_reaches_every_workspace() {
        let mut all = Workspaces::default();
        all.create("one", &names(&["api", "web"])).expect("created");
        all.create("two", &names(&["api"])).expect("created");
        all.rename_project("api", "core");
        assert_eq!(
            all.find("one").map(|w| w.projects.clone()),
            Some(names(&["core", "web"]))
        );
        assert_eq!(
            all.find("two").map(|w| w.projects.clone()),
            Some(names(&["core"]))
        );
    }

    #[test]
    fn a_removed_project_leaves_every_workspace() {
        let mut all = Workspaces::default();
        all.create("one", &names(&["api", "web"])).expect("created");
        all.drop_project("api");
        assert_eq!(
            all.find("one").map(|w| w.projects.clone()),
            Some(names(&["web"]))
        );
    }

    #[test]
    fn set_projects_replaces_the_list_in_its_new_order() {
        let mut all = Workspaces::default();
        all.create("w", &names(&["a", "b"])).expect("created");
        all.set_projects("w", &names(&["c", "a"]))
            .expect("replaced");
        assert_eq!(
            all.find("w").map(|w| w.projects.clone()),
            Some(names(&["c", "a"]))
        );
    }
}
