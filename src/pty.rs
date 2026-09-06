//! The process boundary. Everything that starts a real child process goes through
//! `PtySpawner`, so tests can substitute a fake and never touch the operating system.

use std::fmt;
use std::io::{Read, Write};
use std::path::PathBuf;

use anyhow::Result;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

/// What to run, where, and at what size.
#[derive(Debug, Clone)]
pub struct SessionSpec {
    pub name: String,
    pub program: String,
    pub args: Vec<String>,
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
    pub fn with_size(mut self, rows: u16, cols: u16) -> Self {
        self.rows = rows;
        self.cols = cols;
        self
    }
}

/// The write half of a running session.
pub trait Pty: fmt::Debug + Send {
    fn write(&mut self, data: &[u8]) -> Result<()>;
    fn resize(&mut self, rows: u16, cols: u16) -> Result<()>;
}

/// Starts sessions. The real implementation opens a pseudoterminal. Tests use a fake.
pub trait PtySpawner: fmt::Debug {
    /// Returns the write half and the read half of a new session.
    fn spawn(&self, spec: &SessionSpec) -> Result<(Box<dyn Pty>, Box<dyn Read + Send>)>;
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

impl PtySpawner for SystemPtySpawner {
    fn spawn(&self, spec: &SessionSpec) -> Result<(Box<dyn Pty>, Box<dyn Read + Send>)> {
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

        let mut child = pair.slave.spawn_command(cmd)?;
        // Release the slave, otherwise the master never reaches end of file. See FINDINGS.md.
        drop(pair.slave);

        let reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        std::thread::spawn(move || {
            let _ = child.wait();
        });

        Ok((
            Box::new(SystemPty {
                master: pair.master,
                writer,
            }),
            reader,
        ))
    }
}
