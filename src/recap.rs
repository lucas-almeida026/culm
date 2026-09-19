//! Asking a paused session what it was doing.
//!
//! Claude Code ships a `/recap` command that answers in a sentence or two, and it
//! runs without a terminal. culm harvests it after the child is gone, so nothing is
//! typed into a live session and no terminal output is ever read.
//!
//! Starting a process is a side effect, so it enters through a trait. The work is
//! polled rather than waited on, the way `PtySpawner` hands back a `Child`, because
//! the answer takes seconds and the interface must keep drawing.

use std::fmt;
use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use anyhow::{Result, anyhow, bail};

/// What culm keeps when the recap could not be had.
///
/// The reason is never the user's problem, so every failure reads the same.
pub const RECAP_UNAVAILABLE: &str = "no recap was generated for this session";

/// Starts the work of recapping one session.
pub trait Recapper: fmt::Debug {
    /// `cwd` is the directory the session ran in, which is how Claude Code finds
    /// the transcript that belongs to the id.
    fn start(&self, session_id: &str, cwd: &Path) -> Result<Box<dyn RecapJob>>;
}

/// Work in flight. The loop polls it until it answers.
pub trait RecapJob: fmt::Debug {
    /// `None` while the work is still running.
    fn poll(&mut self) -> Option<Result<String>>;
    /// Gives up on the work. The caller enforces the time limit, not the job.
    fn kill(&mut self);
}

/// Runs a headless Claude Code child against a session that has already stopped.
#[derive(Debug, Default, Clone, Copy)]
pub struct ClaudeRecapper;

impl Recapper for ClaudeRecapper {
    fn start(&self, session_id: &str, cwd: &Path) -> Result<Box<dyn RecapJob>> {
        // `--no-session-persistence` is what keeps the transcript untouched. Without
        // it, `--resume` appends the recap exchange to the real conversation and
        // moves the file's time, which culm itself reads as the last use.
        let child = Command::new("claude")
            .args(["-p", "--no-session-persistence", "--resume", session_id])
            .arg("/recap")
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        Ok(Box::new(ClaudeRecapJob { child }))
    }
}

#[derive(Debug)]
struct ClaudeRecapJob {
    child: Child,
}

impl RecapJob for ClaudeRecapJob {
    fn poll(&mut self) -> Option<Result<String>> {
        match self.child.try_wait() {
            Ok(None) => None,
            Ok(Some(status)) if status.success() => Some(read_answer(&mut self.child)),
            Ok(Some(status)) => Some(Err(anyhow!("claude exited with {status}"))),
            Err(e) => Some(Err(e.into())),
        }
    }

    fn kill(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
    }
}

/// Reads what the child printed, once it has ended.
///
/// The answer is a sentence or two, far below a pipe's capacity, so nothing is read
/// until the child is gone and the write end is closed.
fn read_answer(child: &mut Child) -> Result<String> {
    let Some(mut out) = child.stdout.take() else {
        bail!("claude wrote nothing");
    };
    let mut text = String::new();
    out.read_to_string(&mut text)?;
    let text = clean(&text);
    if text.is_empty() {
        bail!("claude returned no recap");
    }
    Ok(text)
}

/// Trims the answer and folds it into one paragraph.
///
/// A recap with no turns behind it reads as a refusal rather than a summary, and
/// keeping that sentence would be worse than keeping nothing, so it is refused here.
#[must_use]
pub fn clean(text: &str) -> String {
    let joined = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if is_refusal(&joined) {
        return String::new();
    }
    joined
}

/// True when Claude Code answered that it had nothing to work from.
fn is_refusal(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.starts_with("nothing to recap")
        || lower.starts_with("recap cancelled")
        || lower.starts_with("couldn't generate a recap")
}

#[cfg(test)]
mod tests {
    // Test code may use `expect` with a message. Library code may not.
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn an_answer_is_folded_into_one_paragraph() {
        assert_eq!(
            clean("  first line\n\nsecond line  \n"),
            "first line second line"
        );
    }

    #[test]
    fn nothing_to_recap_is_not_an_answer() {
        assert_eq!(clean("Nothing to recap yet — send a message first."), "");
    }

    #[test]
    fn a_cancelled_recap_is_not_an_answer() {
        assert_eq!(clean("Recap cancelled."), "");
    }

    #[test]
    fn a_failed_recap_is_not_an_answer() {
        assert_eq!(
            clean("Couldn't generate a recap. Run with --debug for details."),
            ""
        );
    }

    #[test]
    fn a_real_recap_survives() {
        let answer = "Goal: refinar o CLUB-1137. Próximo passo: fechar o desenho.";
        assert_eq!(clean(answer), answer);
    }
}
