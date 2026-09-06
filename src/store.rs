//! Persistence. Every read and write of a file goes through `Store`, so tests keep
//! state in memory and touch no disk.

use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;

use crate::project::{Project, Registry};

/// The state culm keeps between runs, and the Claude Code settings it edits.
pub trait Store: fmt::Debug {
    fn load_registry(&self) -> Result<Registry>;
    fn save_registry(&self, registry: &Registry) -> Result<()>;
    /// Returns an empty project when the slug has no saved state yet.
    fn load_project(&self, slug: &str) -> Result<Option<Project>>;
    fn save_project(&self, project: &Project) -> Result<()>;
    fn read_claude_settings(&self) -> Result<Value>;
    fn write_claude_settings(&self, settings: &Value) -> Result<()>;
}

/// Reads and writes real files under the state directory.
#[derive(Debug, Clone)]
pub struct FsStore {
    state_dir: PathBuf,
    claude_settings: PathBuf,
}

impl FsStore {
    /// Uses `$XDG_STATE_HOME/culm`, or `~/.local/state/culm` when the variable is unset.
    pub fn new() -> Result<Self> {
        let home = std::env::var_os("HOME").context("HOME is not set")?;
        let home = PathBuf::from(home);
        let state_dir = match std::env::var_os("XDG_STATE_HOME") {
            Some(x) if !x.is_empty() => PathBuf::from(x),
            _ => home.join(".local/state"),
        }
        .join("culm");
        Ok(Self {
            state_dir,
            claude_settings: home.join(".claude/settings.json"),
        })
    }

    #[must_use]
    pub fn at(state_dir: impl Into<PathBuf>, claude_settings: impl Into<PathBuf>) -> Self {
        Self {
            state_dir: state_dir.into(),
            claude_settings: claude_settings.into(),
        }
    }

    fn registry_path(&self) -> PathBuf {
        self.state_dir.join("registry.json")
    }

    fn project_path(&self, slug: &str) -> PathBuf {
        self.state_dir.join("projects").join(format!("{slug}.json"))
    }
}

/// Writes through a temporary file, so an interrupted write never truncates the
/// saved state.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename into {}", path.display()))?;
    Ok(())
}

fn read_json<T: serde::de::DeserializeOwned + Default>(path: &Path) -> Result<T> {
    match std::fs::read(path) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

impl Store for FsStore {
    fn load_registry(&self) -> Result<Registry> {
        read_json(&self.registry_path())
    }

    fn save_registry(&self, registry: &Registry) -> Result<()> {
        write_atomic(&self.registry_path(), &serde_json::to_vec_pretty(registry)?)
    }

    fn load_project(&self, slug: &str) -> Result<Option<Project>> {
        let path = self.project_path(slug);
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(
                serde_json::from_slice(&bytes)
                    .with_context(|| format!("parse {}", path.display()))?,
            )),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
        }
    }

    fn save_project(&self, project: &Project) -> Result<()> {
        write_atomic(
            &self.project_path(&project.slug),
            &serde_json::to_vec_pretty(project)?,
        )
    }

    fn read_claude_settings(&self) -> Result<Value> {
        match std::fs::read(&self.claude_settings) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("parse {}", self.claude_settings.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok(Value::Object(serde_json::Map::new()))
            }
            Err(e) => Err(e).with_context(|| format!("read {}", self.claude_settings.display())),
        }
    }

    fn write_claude_settings(&self, settings: &Value) -> Result<()> {
        write_atomic(&self.claude_settings, &serde_json::to_vec_pretty(settings)?)
    }
}
