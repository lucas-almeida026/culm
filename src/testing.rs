//! Test doubles. Compiled into the library so that integration tests under `tests/`
//! can use them. Nothing here starts a process or touches the file system.

use std::fmt;
use std::io::Cursor;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};

use crate::pty::{Child, Pty, PtySpawner, SessionSpec, Spawned};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A pseudoterminal that records what was written to it and replays canned output.
#[derive(Debug, Default, Clone)]
pub struct FakePty {
    pub written: Arc<Mutex<Vec<u8>>>,
    pub size: Arc<Mutex<(u16, u16)>>,
}

impl FakePty {
    #[must_use]
    pub fn written_utf8(&self) -> String {
        String::from_utf8_lossy(&lock(&self.written)).into_owned()
    }

    #[must_use]
    pub fn size(&self) -> (u16, u16) {
        *lock(&self.size)
    }
}

impl Pty for FakePty {
    fn write(&mut self, data: &[u8]) -> Result<()> {
        lock(&self.written).extend_from_slice(data);
        Ok(())
    }

    fn resize(&mut self, rows: u16, cols: u16) -> Result<()> {
        *lock(&self.size) = (rows, cols);
        Ok(())
    }
}

/// A child process that records the signals it received. Every clone shares the
/// state, so a test holds one handle while the session holds another.
#[derive(Debug, Clone)]
pub struct FakeChild {
    pid: u32,
    terminated: Arc<AtomicBool>,
    killed: Arc<AtomicBool>,
    exited: Arc<AtomicBool>,
}

impl FakeChild {
    #[must_use]
    fn new(pid: u32) -> Self {
        Self {
            pid,
            terminated: Arc::new(AtomicBool::new(false)),
            killed: Arc::new(AtomicBool::new(false)),
            exited: Arc::new(AtomicBool::new(false)),
        }
    }

    #[must_use]
    pub fn terminated(&self) -> bool {
        self.terminated.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn killed(&self) -> bool {
        self.killed.load(Ordering::Relaxed)
    }

    /// Reports that the child ended on its own, as a real child does when the user
    /// types `/exit`.
    pub fn exit_on_its_own(&self) {
        self.exited.store(true, Ordering::Relaxed);
    }
}

impl Child for FakeChild {
    fn pid(&self) -> Option<u32> {
        Some(self.pid)
    }

    fn terminate(&mut self) -> Result<()> {
        self.terminated.store(true, Ordering::Relaxed);
        self.exited.store(true, Ordering::Relaxed);
        Ok(())
    }

    fn kill(&mut self) -> Result<()> {
        self.killed.store(true, Ordering::Relaxed);
        self.exited.store(true, Ordering::Relaxed);
        Ok(())
    }

    fn has_exited(&mut self) -> bool {
        self.exited.load(Ordering::Relaxed)
    }
}

/// What one call to `FakeSpawner::spawn` produced.
#[derive(Debug, Clone)]
pub struct FakeSpawn {
    pub spec: SessionSpec,
    pub pty: FakePty,
    pub child: FakeChild,
}

/// Hands out fake halves and replays a fixed byte stream as session output.
#[derive(Clone, Default)]
pub struct FakeSpawner {
    output: Vec<u8>,
    spawns: Arc<Mutex<Vec<FakeSpawn>>>,
    fail: Arc<AtomicBool>,
}

impl fmt::Debug for FakeSpawner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FakeSpawner")
            .field("spawns", &self.spawns().len())
            .finish_non_exhaustive()
    }
}

impl FakeSpawner {
    #[must_use]
    pub fn new(output: impl Into<Vec<u8>>) -> Self {
        Self {
            output: output.into(),
            spawns: Arc::new(Mutex::new(Vec::new())),
            fail: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Makes every later spawn fail, so a test covers the failure path.
    pub fn fail_from_now_on(&self) {
        self.fail.store(true, Ordering::Relaxed);
    }

    #[must_use]
    pub fn spawns(&self) -> Vec<FakeSpawn> {
        lock(&self.spawns).clone()
    }

    #[must_use]
    pub fn specs(&self) -> Vec<SessionSpec> {
        self.spawns().into_iter().map(|s| s.spec).collect()
    }

    #[must_use]
    pub fn count(&self) -> usize {
        lock(&self.spawns).len()
    }

    /// The pseudoterminal handed to the most recent session.
    #[must_use]
    pub fn last_pty(&self) -> Option<FakePty> {
        self.spawns().last().map(|s| s.pty.clone())
    }

    /// The child handed to the most recent session.
    #[must_use]
    pub fn last_child(&self) -> Option<FakeChild> {
        self.spawns().last().map(|s| s.child.clone())
    }

    /// The spawn whose session name matches, or `None`.
    #[must_use]
    pub fn spawn_named(&self, name: &str) -> Option<FakeSpawn> {
        self.spawns().into_iter().find(|s| s.spec.name == name)
    }
}

impl PtySpawner for FakeSpawner {
    fn spawn(&self, spec: &SessionSpec) -> Result<Spawned> {
        if self.fail.load(Ordering::Relaxed) {
            return Err(anyhow!("fake spawner refused to start {}", spec.name));
        }
        let mut guard = lock(&self.spawns);
        let pty = FakePty::default();
        *lock(&pty.size) = (spec.rows, spec.cols);
        let child = FakeChild::new(1000 + u32::try_from(guard.len()).unwrap_or(0));
        guard.push(FakeSpawn {
            spec: spec.clone(),
            pty: pty.clone(),
            child: child.clone(),
        });
        Ok(Spawned {
            pty: Box::new(pty),
            reader: Box::new(Cursor::new(self.output.clone())),
            child: Box::new(child),
        })
    }
}

/// Records every worktree culm asked for, and fails on demand.
#[derive(Debug, Default, Clone)]
pub struct FakeGit {
    calls: Arc<Mutex<Vec<WorktreeCall>>>,
    removed: Arc<Mutex<Vec<std::path::PathBuf>>>,
    dirty: Arc<Mutex<Vec<std::path::PathBuf>>>,
    fail: Arc<AtomicBool>,
}

/// One call to `Git::worktree_add`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeCall {
    pub repo: std::path::PathBuf,
    pub path: std::path::PathBuf,
    pub branch: String,
}

impl FakeGit {
    #[must_use]
    pub fn calls(&self) -> Vec<WorktreeCall> {
        lock(&self.calls).clone()
    }

    /// Makes every later worktree fail, so a test covers the abort path.
    pub fn fail_from_now_on(&self) {
        self.fail.store(true, Ordering::Relaxed);
    }

    /// Every worktree culm asked git to remove.
    #[must_use]
    pub fn removed(&self) -> Vec<std::path::PathBuf> {
        lock(&self.removed).clone()
    }

    /// Reports this worktree as holding an uncommitted change.
    pub fn mark_dirty(&self, path: impl Into<std::path::PathBuf>) {
        lock(&self.dirty).push(path.into());
    }
}

impl crate::git::Git for FakeGit {
    fn worktree_add(
        &self,
        repo: &std::path::Path,
        path: &std::path::Path,
        branch: &str,
    ) -> Result<()> {
        if self.fail.load(Ordering::Relaxed) {
            return Err(anyhow!("fatal: '{branch}' is already checked out"));
        }
        lock(&self.calls).push(WorktreeCall {
            repo: repo.to_path_buf(),
            path: path.to_path_buf(),
            branch: branch.to_string(),
        });
        Ok(())
    }

    fn worktree_is_dirty(&self, path: &std::path::Path) -> Result<bool> {
        Ok(lock(&self.dirty).iter().any(|p| p == path))
    }

    fn worktree_remove(&self, _repo: &std::path::Path, path: &std::path::Path) -> Result<()> {
        lock(&self.removed).push(path.to_path_buf());
        Ok(())
    }
}

/// Keeps saved state in memory and counts the writes, so a test proves that state
/// reaches disk after every change.
#[derive(Debug, Clone)]
pub struct MemoryStore {
    registry: Arc<Mutex<crate::project::Registry>>,
    projects: Arc<Mutex<std::collections::HashMap<String, crate::project::Project>>>,
    settings: Arc<Mutex<serde_json::Value>>,
    saves: Arc<std::sync::atomic::AtomicUsize>,
    removed_transcripts: Arc<Mutex<Vec<(std::path::PathBuf, String)>>>,
    claude_dirs: Arc<Mutex<Vec<String>>>,
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self {
            registry: Arc::new(Mutex::new(crate::project::Registry::default())),
            projects: Arc::new(Mutex::new(std::collections::HashMap::new())),
            settings: Arc::new(Mutex::new(serde_json::json!({}))),
            saves: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            removed_transcripts: Arc::new(Mutex::new(Vec::new())),
            claude_dirs: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl MemoryStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How many times a project was written.
    #[must_use]
    pub fn saves(&self) -> usize {
        self.saves.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn project(&self, slug: &str) -> Option<crate::project::Project> {
        lock(&self.projects).get(slug).cloned()
    }

    pub fn put_project(&self, project: &crate::project::Project) {
        lock(&self.projects).insert(project.slug.clone(), project.clone());
    }

    pub fn put_settings(&self, settings: serde_json::Value) {
        *lock(&self.settings) = settings;
    }

    /// Seeds the listing of `~/.claude/projects`.
    pub fn put_claude_dirs<I, S>(&self, dirs: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        *lock(&self.claude_dirs) = dirs.into_iter().map(Into::into).collect();
    }

    /// Every transcript culm asked to delete, as the working directory and the id.
    #[must_use]
    pub fn removed_transcripts(&self) -> Vec<(std::path::PathBuf, String)> {
        lock(&self.removed_transcripts).clone()
    }
}

impl crate::store::Store for MemoryStore {
    fn load_registry(&self) -> Result<crate::project::Registry> {
        Ok(lock(&self.registry).clone())
    }

    fn save_registry(&self, registry: &crate::project::Registry) -> Result<()> {
        *lock(&self.registry) = registry.clone();
        Ok(())
    }

    fn load_project(&self, slug: &str) -> Result<Option<crate::project::Project>> {
        Ok(lock(&self.projects).get(slug).cloned())
    }

    fn save_project(&self, project: &crate::project::Project) -> Result<()> {
        self.saves.fetch_add(1, Ordering::Relaxed);
        lock(&self.projects).insert(project.slug.clone(), project.clone());
        Ok(())
    }

    fn read_claude_settings(&self) -> Result<serde_json::Value> {
        Ok(lock(&self.settings).clone())
    }

    fn write_claude_settings(&self, settings: &serde_json::Value) -> Result<()> {
        *lock(&self.settings) = settings.clone();
        Ok(())
    }

    fn remove_transcript(&self, cwd: &std::path::Path, id: &str) -> Result<()> {
        lock(&self.removed_transcripts).push((cwd.to_path_buf(), id.to_string()));
        Ok(())
    }

    fn remove_project(&self, slug: &str) -> Result<()> {
        lock(&self.projects).remove(slug);
        Ok(())
    }

    fn list_claude_dirs(&self) -> Result<Vec<String>> {
        Ok(lock(&self.claude_dirs).clone())
    }

    fn remove_claude_dir(&self, name: &str) -> Result<()> {
        lock(&self.claude_dirs).retain(|d| d != name);
        Ok(())
    }
}

/// A clock a test moves by hand, so an ordering rule never waits for real time.
#[derive(Debug, Default, Clone)]
pub struct FakeClock {
    secs: Arc<AtomicU64>,
}

impl FakeClock {
    #[must_use]
    pub fn new(secs: u64) -> Self {
        Self {
            secs: Arc::new(AtomicU64::new(secs)),
        }
    }

    pub fn set(&self, secs: u64) {
        self.secs.store(secs, Ordering::Relaxed);
    }
}

impl crate::clock::Clock for FakeClock {
    fn now_secs(&self) -> u64 {
        self.secs.load(Ordering::Relaxed)
    }
}

/// Reports a fixed memory figure per process, so a test never reads `/proc`.
#[derive(Debug, Default, Clone)]
pub struct FakeMemoryProbe {
    per_pid: Arc<Mutex<std::collections::HashMap<u32, u64>>>,
    fallback: u64,
}

impl FakeMemoryProbe {
    #[must_use]
    pub fn new(fallback: u64) -> Self {
        Self {
            per_pid: Arc::new(Mutex::new(std::collections::HashMap::new())),
            fallback,
        }
    }

    pub fn set(&self, pid: u32, bytes: u64) {
        lock(&self.per_pid).insert(pid, bytes);
    }
}

impl crate::stats::MemoryProbe for FakeMemoryProbe {
    fn rss_tree(&self, pid: u32) -> u64 {
        lock(&self.per_pid)
            .get(&pid)
            .copied()
            .unwrap_or(self.fallback)
    }
}
