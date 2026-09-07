//! Naming an imported session. Starting a process is a side effect, so it enters
//! through a trait and a test supplies a fake.
//!
//! A transcript that was never renamed inside Claude Code carries no name. The first
//! thing the user asked is the only description of it that exists, so a small model
//! reads that and answers with a name.

use std::fmt;
use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{Result, bail};

/// Turns the first prompt of a transcript into a session name.
pub trait Namer: fmt::Debug {
    fn name_for(&self, first_prompt: &str) -> Result<String>;
}

/// Asks a headless Claude Code child, on the smallest model.
#[derive(Debug, Default, Clone, Copy)]
pub struct ClaudeNamer;

/// The whole instruction. The prompt follows it on standard input.
const INSTRUCTION: &str = "Reply with only a session name of three to six lower-case \
     words, no punctuation, that describes this request.";

impl Namer for ClaudeNamer {
    fn name_for(&self, first_prompt: &str) -> Result<String> {
        // `--bare` skips the hooks, so naming a session never posts a marker.
        // `--tools ""` leaves the child no way to touch the machine.
        let mut child = Command::new("claude")
            .args([
                "-p",
                "--bare",
                "--model",
                "haiku",
                "--no-session-persistence",
                "--tools",
                "",
                "--output-format",
                "text",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            write!(stdin, "{INSTRUCTION}\n\n{first_prompt}")?;
        }
        let output = child.wait_with_output()?;
        if !output.status.success() {
            bail!("claude exited with {}", output.status);
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let name = text
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or_default()
            .to_string();
        if name.is_empty() {
            bail!("claude returned no name");
        }
        Ok(name)
    }
}
