//! The process boundary. Everything that starts a real child process goes through
//! `PtySpawner`, so tests can substitute a fake and never touch the operating system.

use std::fmt;
use std::io::{Read, Write};
use std::path::PathBuf;

use anyhow::Result;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

/// What to run, where, with what environment, and at what size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSpec {
    pub name: String,
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: PathBuf,
    pub rows: u16,
    pub cols: u16,
}

impl SessionSpec {
    pub fn new(
        name: impl Into<String>,
        program: impl Into<String>,
        cwd: impl Into<PathBuf>,
    ) -> Self {
        Self {
            name: name.into(),
            program: program.into(),
            args: Vec::new(),
            env: Vec::new(),
            cwd: cwd.into(),
            rows: 24,
            cols: 80,
        }
    }

    #[must_use]
    pub fn with_args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    #[must_use]
    pub fn with_env<I, K, V>(mut self, env: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.env = env.into_iter().map(|(k, v)| (k.into(), v.into())).collect();
        self
    }

    #[must_use]
    pub fn with_size(mut self, rows: u16, cols: u16) -> Self {
        self.rows = rows;
        self.cols = cols;
        self
    }

    /// True when the argument list holds `flag` followed by `value`.
    #[must_use]
    pub fn has_flag(&self, flag: &str, value: &str) -> bool {
        self.args.windows(2).any(|w| w[0] == flag && w[1] == value)
    }
}

/// The write half of a running session.
pub trait Pty: fmt::Debug + Send {
    fn write(&mut self, data: &[u8]) -> Result<()>;
    fn resize(&mut self, rows: u16, cols: u16) -> Result<()>;
}

/// The process half of a running session. A pause needs a way to end a child, and
/// the tick needs a way to learn that a child ended on its own.
pub trait Child: fmt::Debug + Send {
    fn pid(&self) -> Option<u32>;
    /// Asks the child to exit. SIGTERM for a real child.
    fn terminate(&mut self) -> Result<()>;
    /// Ends the child without asking. SIGKILL for a real child.
    fn kill(&mut self) -> Result<()>;
    /// True once the child has ended. Reaps the child when it has.
    fn has_exited(&mut self) -> bool;
}

/// The three halves of a started session.
pub struct Spawned {
    pub pty: Box<dyn Pty>,
    pub reader: Box<dyn Read + Send>,
    pub child: Box<dyn Child>,
}

impl fmt::Debug for Spawned {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Spawned")
            .field("pty", &self.pty)
            .field("child", &self.child)
            .finish_non_exhaustive()
    }
}

/// Starts sessions. The real implementation opens a pseudoterminal. Tests use a fake.
pub trait PtySpawner: fmt::Debug {
    fn spawn(&self, spec: &SessionSpec) -> Result<Spawned>;
}

/// Spawns a real child process on a real pseudoterminal.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemPtySpawner;

struct SystemPty {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
}

impl fmt::Debug for SystemPty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SystemPty")
    }
}

impl Pty for SystemPty {
    fn write(&mut self, data: &[u8]) -> Result<()> {
        self.writer.write_all(data)?;
        self.writer.flush()?;
        Ok(())
    }

    fn resize(&mut self, rows: u16, cols: u16) -> Result<()> {
        self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        Ok(())
    }
}

#[derive(Debug)]
struct SystemChild {
    inner: Box<dyn portable_pty::Child + Send + Sync>,
    exited: bool,
}

impl Child for SystemChild {
    fn pid(&self) -> Option<u32> {
        self.inner.process_id()
    }

    fn terminate(&mut self) -> Result<()> {
        let Some(pid) = self.pid() else {
            return Ok(());
        };
        // portable-pty only offers SIGKILL. A pause asks first, so send SIGTERM here
        // and let the caller escalate.
        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(i32::try_from(pid)?),
            nix::sys::signal::Signal::SIGTERM,
        )?;
        Ok(())
    }

    fn kill(&mut self) -> Result<()> {
        self.inner.kill()?;
        Ok(())
    }

    fn has_exited(&mut self) -> bool {
        if self.exited {
            return true;
        }
        if let Ok(Some(_)) = self.inner.try_wait() {
            self.exited = true;
        }
        self.exited
    }
}

impl PtySpawner for SystemPtySpawner {
    fn spawn(&self, spec: &SessionSpec) -> Result<Spawned> {
        let pair = native_pty_system().openpty(PtySize {
            rows: spec.rows,
            cols: spec.cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let mut cmd = CommandBuilder::new(&spec.program);
        for a in &spec.args {
            cmd.arg(a);
        }
        cmd.cwd(&spec.cwd);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }

        let child = pair.slave.spawn_command(cmd)?;
        // Release the slave, otherwise the master never reaches end of file. See FINDINGS.md.
        drop(pair.slave);

        let reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;

        Ok(Spawned {
            pty: Box::new(SystemPty {
                master: pair.master,
                writer,
            }),
            reader,
            child: Box::new(SystemChild {
                inner: child,
                exited: false,
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_flag_finds_a_flag_and_its_value() {
        let spec = SessionSpec::new("s", "claude", "/tmp").with_args([
            "--resume",
            "abc",
            "--add-dir",
            "/root",
        ]);
        assert!(spec.has_flag("--resume", "abc"));
        assert!(spec.has_flag("--add-dir", "/root"));
        assert!(!spec.has_flag("--resume", "/root"));
        assert!(!spec.has_flag("--session-id", "abc"));
    }
}
