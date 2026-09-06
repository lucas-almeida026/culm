//! Test doubles. Compiled into the library so that integration tests under `tests/`
//! can use them. Nothing here starts a process or touches the file system.

use std::fmt;
use std::io::{Cursor, Read};
use std::sync::{Arc, Mutex};

use anyhow::Result;

use crate::pty::{Pty, PtySpawner, SessionSpec};

/// A pseudoterminal that records what was written to it and replays canned output.
#[derive(Debug, Default, Clone)]
pub struct FakePty {
    pub written: Arc<Mutex<Vec<u8>>>,
    pub size: Arc<Mutex<(u16, u16)>>,
}

impl FakePty {
    #[must_use]
    pub fn written_utf8(&self) -> String {
        let guard = self.written.lock().unwrap_or_else(|e| e.into_inner());
        String::from_utf8_lossy(&guard).into_owned()
    }

    #[must_use]
    pub fn size(&self) -> (u16, u16) {
        *self.size.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Pty for FakePty {
    fn write(&mut self, data: &[u8]) -> Result<()> {
        self.written
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend_from_slice(data);
        Ok(())
    }

    fn resize(&mut self, rows: u16, cols: u16) -> Result<()> {
        *self.size.lock().unwrap_or_else(|e| e.into_inner()) = (rows, cols);
        Ok(())
    }
}

/// Hands out `FakePty` values and replays a fixed byte stream as session output.
#[derive(Clone, Default)]
pub struct FakeSpawner {
    output: Vec<u8>,
    pub last: Arc<Mutex<Option<FakePty>>>,
}

impl fmt::Debug for FakeSpawner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FakeSpawner")
    }
}

impl FakeSpawner {
    #[must_use]
    pub fn new(output: impl Into<Vec<u8>>) -> Self {
        Self {
            output: output.into(),
            last: Arc::new(Mutex::new(None)),
        }
    }

    /// The pseudoterminal handed to the most recent session.
    #[must_use]
    pub fn last_pty(&self) -> Option<FakePty> {
        self.last.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl PtySpawner for FakeSpawner {
    fn spawn(&self, _spec: &SessionSpec) -> Result<(Box<dyn Pty>, Box<dyn Read + Send>)> {
        let pty = FakePty::default();
        *self.last.lock().unwrap_or_else(|e| e.into_inner()) = Some(pty.clone());
        Ok((Box::new(pty), Box::new(Cursor::new(self.output.clone()))))
    }
}
