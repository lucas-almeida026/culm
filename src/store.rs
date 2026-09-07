//! Persistence. Every read and write of a file goes through `Store`, so tests keep
//! state in memory and touch no disk.

use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;

use crate::project::{Project, Registry, transcript_dir_name};
use crate::transcript::is_session_file;

/// One transcript file under `~/.claude/projects/<dir>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptFile {
    /// The Claude Code session id, which is the file name without its extension.
    pub id: String,
    /// Modification time, in seconds since the unix epoch. An import uses it as the
    /// time the session was last active.
    pub modified_secs: u64,
}

/// The state culm keeps between runs, and the Claude Code settings it edits.
pub trait Store: fmt::Debug {
    fn load_registry(&self) -> Result<Registry>;
    fn save_registry(&self, registry: &Registry) -> Result<()>;
    /// Returns an empty project when the slug has no saved state yet.
    fn load_project(&self, slug: &str) -> Result<Option<Project>>;
    fn save_project(&self, project: &Project) -> Result<()>;
    fn read_claude_settings(&self) -> Result<Value>;
    fn write_claude_settings(&self, settings: &Value) -> Result<()>;
    /// Removes the transcript of one session. Deletion has no undo.
    fn remove_transcript(&self, cwd: &Path, id: &str) -> Result<()>;
    /// Forgets a project's own saved state. Deletes nothing the project holds.
    fn remove_project(&self, slug: &str) -> Result<()>;
    /// The directory names under `~/.claude/projects`.
    fn list_claude_dirs(&self) -> Result<Vec<String>>;
    /// The session files of one directory under `~/.claude/projects`.
    fn list_transcripts(&self, dir: &str) -> Result<Vec<TranscriptFile>>;
    /// The whole text of one transcript.
    fn read_transcript(&self, dir: &str, id: &str) -> Result<String>;
    /// Removes one whole directory under `~/.claude/projects`.
    fn remove_claude_dir(&self, name: &str) -> Result<()>;
}

/// Reads and writes real files under the state directory.
#[derive(Debug, Clone)]
pub struct FsStore {
    state_dir: PathBuf,
    claude_settings: PathBuf,
    claude_projects: PathBuf,
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
            claude_projects: home.join(".claude/projects"),
        })
    }

    #[must_use]
    pub fn at(
        state_dir: impl Into<PathBuf>,
        claude_settings: impl Into<PathBuf>,
        claude_projects: impl Into<PathBuf>,
    ) -> Self {
        Self {
            state_dir: state_dir.into(),
            claude_settings: claude_settings.into(),
            claude_projects: claude_projects.into(),
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

    fn remove_project(&self, slug: &str) -> Result<()> {
        match std::fs::remove_file(self.project_path(slug)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("remove state of {slug}")),
        }
    }

    fn list_claude_dirs(&self) -> Result<Vec<String>> {
        let entries = match std::fs::read_dir(&self.claude_projects) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => {
                return Err(e).with_context(|| format!("read {}", self.claude_projects.display()));
            }
        };
        Ok(entries
            .flatten()
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect())
    }

    fn list_transcripts(&self, dir: &str) -> Result<Vec<TranscriptFile>> {
        let path = self.claude_projects.join(dir);
        let entries = match std::fs::read_dir(&path) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
        };
        let mut files = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !is_session_file(&name) {
                continue;
            }
            // A file whose time cannot be read still imports. It sorts last, which is
            // where a session of unknown age belongs.
            let modified_secs = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            files.push(TranscriptFile {
                id: name.trim_end_matches(".jsonl").to_string(),
                modified_secs,
            });
        }
        Ok(files)
    }

    fn read_transcript(&self, dir: &str, id: &str) -> Result<String> {
        let path = self.claude_projects.join(dir).join(format!("{id}.jsonl"));
        // A transcript can hold bytes that are not valid text, and a name is worth
        // more than a strict read, so the invalid bytes are replaced.
        let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    fn remove_claude_dir(&self, name: &str) -> Result<()> {
        // A name is always one path segment from `list_claude_dirs`, so it cannot
        // escape the projects directory.
        let path = self.claude_projects.join(name);
        match std::fs::remove_dir_all(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("remove {}", path.display())),
        }
    }

    fn remove_transcript(&self, cwd: &Path, id: &str) -> Result<()> {
        let path = self
            .claude_projects
            .join(transcript_dir_name(cwd))
            .join(format!("{id}.jsonl"));
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            // A session that never wrote a turn has no transcript, and that is not a
            // failure to delete it.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("remove {}", path.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    // Test code may use `expect` with a message. Library code may not.
    #![allow(clippy::expect_used)]

    use super::*;

    /// A directory of its own per test, inside the system temporary directory.
    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("culm-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn store_at(root: &Path) -> FsStore {
        FsStore::at(
            root.join("state"),
            root.join("claude/settings.json"),
            root.join("claude/projects"),
        )
    }

    #[test]
    fn remove_transcript_deletes_only_the_named_session() {
        let root = temp_dir("transcripts");
        let store = store_at(&root);
        let cwd = Path::new("/home/x/spm/.worktrees/api-mate-feat-a");
        let dir = root.join("claude/projects").join(transcript_dir_name(cwd));
        std::fs::create_dir_all(&dir).expect("project dir");
        let target = dir.join("keep-me-not.jsonl");
        let sibling = dir.join("another-session.jsonl");
        std::fs::write(&target, b"{}").expect("write");
        std::fs::write(&sibling, b"{}").expect("write");

        store
            .remove_transcript(cwd, "keep-me-not")
            .expect("remove succeeds");

        assert!(!target.exists(), "the named transcript is gone");
        assert!(
            sibling.exists(),
            "another session in the same directory survives"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn removing_a_transcript_that_never_existed_is_not_a_failure() {
        let root = temp_dir("missing");
        let store = store_at(&root);
        store
            .remove_transcript(Path::new("/home/x/spm"), "never-wrote-a-turn")
            .expect("a session with no transcript still deletes");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_project_survives_a_round_trip_through_real_files() {
        let root = temp_dir("roundtrip");
        let store = store_at(&root);
        let project = Project::new("spm", "/home/x/spm");
        store.save_project(&project).expect("save");

        let loaded = store.load_project("spm").expect("load").expect("exists");

        assert_eq!(loaded, project);
        assert!(store.load_project("absent").expect("load").is_none());
        std::fs::remove_dir_all(&root).ok();
    }
}
