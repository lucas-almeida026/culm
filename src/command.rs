//! What a session tells culm about itself: its ticket, its port number, and the
//! worktrees it works through.
//!
//! The command line sends a `Command`. A window that has the project open applies
//! it, and so does the command line itself when no window has it open. Both call
//! `apply`, so one set of rules holds in both places. `apply` is pure, so every rule
//! here is unit tested.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::project::{PORT_NUMBERS, SessionRecord, SessionRepo, SessionState};

/// One question or change about the sessions of one project.
///
/// `session` names a session by its id, its slug, or its name, in that order. Inside
/// a session, the command line fills it from `CULM_SESSION`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Command {
    /// Every session that is not archived, with what it registered.
    List,
    /// What one session registered.
    Show { session: String },
    /// Sets the ticket, or clears it with `None`.
    Ticket {
        session: String,
        ticket: Option<String>,
    },
    /// Sets the port number, or clears it with `None`.
    Port { session: String, port: Option<u8> },
    /// Each port number, and the sessions that hold it.
    Ports,
    /// Registers a worktree. The caller has already asked git what it is.
    WorktreeAdd {
        session: String,
        worktree: SessionRepo,
    },
    /// Forgets a worktree. The worktree itself stays on disk.
    WorktreeRm { session: String, path: PathBuf },
}

/// The answer to one command. `text` is what the command line prints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reply {
    pub ok: bool,
    pub text: String,
    /// True when a record changed, so the caller saves.
    #[serde(default)]
    pub changed: bool,
}

impl Reply {
    fn done(text: impl Into<String>) -> Self {
        Self {
            ok: true,
            text: text.into(),
            changed: false,
        }
    }

    fn changed(text: impl Into<String>) -> Self {
        Self {
            ok: true,
            text: text.into(),
            changed: true,
        }
    }

    #[must_use]
    pub fn refused(text: impl Into<String>) -> Self {
        Self {
            ok: false,
            text: text.into(),
            changed: false,
        }
    }
}

/// The session `selector` names: an id first, then a slug, then a name.
fn find(records: &[&mut SessionRecord], selector: &str) -> Option<usize> {
    records
        .iter()
        .position(|r| r.id == selector)
        .or_else(|| records.iter().position(|r| r.slug == selector))
        .or_else(|| records.iter().position(|r| r.name == selector))
}

fn state_word(state: SessionState) -> &'static str {
    match state {
        SessionState::Active => "active",
        SessionState::Paused => "paused",
        SessionState::Archived => "archived",
    }
}

/// The sessions other than `except` that hold `port` and are not archived. An
/// archived session gives its number back until it is restored.
fn holders(records: &[&mut SessionRecord], port: u8, except: Option<usize>) -> Vec<String> {
    records
        .iter()
        .enumerate()
        .filter(|(i, r)| Some(*i) != except && !r.is_archived() && r.port == Some(port))
        .map(|(_, r)| r.name.clone())
        .collect()
}

fn describe(record: &SessionRecord) -> String {
    let mut s = format!("{}  ({})\n", record.name, state_word(record.state));
    s.push_str(&format!(
        "  ticket    {}\n",
        record.ticket.as_deref().unwrap_or("-")
    ));
    s.push_str(&format!(
        "  port      {}\n",
        record
            .port
            .map_or_else(|| "-".to_string(), |p| p.to_string())
    ));
    if record.repos.is_empty() {
        s.push_str("  worktree  -\n");
    }
    for repo in &record.repos {
        let branch = if repo.branch.is_empty() {
            "detached"
        } else {
            &repo.branch
        };
        s.push_str(&format!(
            "  worktree  {}  {}  {branch}\n",
            repo.name,
            repo.worktree.display()
        ));
    }
    s
}

/// Applies one command to the sessions of one project.
///
/// A refusal changes nothing. A port number another session holds is accepted,
/// because two sessions that never run their servers at the same time may share
/// one. The reply names the other holders, so the caller knows.
#[must_use]
pub fn apply(records: &mut [&mut SessionRecord], command: &Command) -> Reply {
    match command {
        Command::List => {
            let mut text = String::new();
            for r in records.iter().filter(|r| !r.is_archived()) {
                text.push_str(&format!(
                    "{:<36} {:<7} {:<12} {:<4} {}\n",
                    r.name,
                    state_word(r.state),
                    r.ticket.as_deref().unwrap_or("-"),
                    r.port.map_or_else(|| "-".to_string(), |p| p.to_string()),
                    match r.repos.len() {
                        0 => "-".to_string(),
                        1 => "1 worktree".to_string(),
                        n => format!("{n} worktrees"),
                    }
                ));
            }
            if text.is_empty() {
                text.push_str("this project has no session\n");
            }
            Reply::done(text)
        }
        Command::Ports => {
            let mut text = String::new();
            for port in 0..PORT_NUMBERS {
                let held = holders(records, port, None);
                let who = if held.is_empty() {
                    "free".to_string()
                } else {
                    held.join(", ")
                };
                text.push_str(&format!("{port}  {who}\n"));
            }
            Reply::done(text)
        }
        Command::Show { session } => match find(records, session) {
            Some(i) => Reply::done(describe(&*records[i])),
            None => no_session(session),
        },
        Command::Ticket { session, ticket } => {
            let Some(i) = find(records, session) else {
                return no_session(session);
            };
            let ticket = ticket.as_deref().map(str::trim);
            if ticket == Some("") {
                return Reply::refused("a ticket needs at least one character");
            }
            let record = &mut *records[i];
            let ticket = ticket.map(str::to_string);
            if record.ticket == ticket {
                return Reply::done(format!("{} already has that ticket", record.name));
            }
            record.ticket = ticket;
            Reply::changed(match &record.ticket {
                Some(t) => format!("{} works on {t}", record.name),
                None => format!("{} has no ticket", record.name),
            })
        }
        Command::Port { session, port } => {
            let Some(i) = find(records, session) else {
                return no_session(session);
            };
            if let Some(p) = port
                && *p >= PORT_NUMBERS
            {
                return Reply::refused(format!(
                    "a port number is a digit from 0 to {}",
                    PORT_NUMBERS - 1
                ));
            }
            let others = port.map(|p| holders(records, p, Some(i)));
            let record = &mut *records[i];
            if record.port == *port {
                return Reply::done(format!("{} already has that port number", record.name));
            }
            record.port = *port;
            let mut text = match port {
                Some(p) => format!("{} uses port number {p}", record.name),
                None => format!("{} has no port number", record.name),
            };
            if let (Some(p), Some(others)) = (port, others)
                && !others.is_empty()
            {
                text.push_str(&format!(". {} also uses {p}", others.join(", ")));
            }
            Reply::changed(text)
        }
        Command::WorktreeAdd { session, worktree } => {
            let Some(i) = find(records, session) else {
                return no_session(session);
            };
            let record = &mut *records[i];
            if let Some(known) = record
                .repos
                .iter_mut()
                .find(|r| r.worktree == worktree.worktree)
            {
                if *known == *worktree {
                    return Reply::done(format!(
                        "{} already works through {}",
                        record.name,
                        worktree.worktree.display()
                    ));
                }
                // The branch or the name moved since it was registered.
                *known = worktree.clone();
                return Reply::changed(format!(
                    "{} works through {} ({})",
                    record.name,
                    worktree.worktree.display(),
                    worktree.name
                ));
            }
            record.repos.push(worktree.clone());
            Reply::changed(format!(
                "{} works through {} ({})",
                record.name,
                worktree.worktree.display(),
                worktree.name
            ))
        }
        Command::WorktreeRm { session, path } => {
            let Some(i) = find(records, session) else {
                return no_session(session);
            };
            let record = &mut *records[i];
            let before = record.repos.len();
            record.repos.retain(|r| r.worktree != *path);
            if record.repos.len() == before {
                return Reply::refused(format!(
                    "{} has no worktree at {}. run: culm session show",
                    record.name,
                    path.display()
                ));
            }
            Reply::changed(format!(
                "{} no longer works through {}. the worktree is still on disk.",
                record.name,
                path.display()
            ))
        }
    }
}

fn no_session(selector: &str) -> Reply {
    Reply::refused(format!(
        "this project has no session {selector}. run: culm session list"
    ))
}

#[cfg(test)]
mod tests {
    // Test code may use `expect` with a message. Library code may not.
    #![allow(clippy::expect_used)]

    use super::*;

    fn record(id: &str, name: &str) -> SessionRecord {
        SessionRecord {
            id: id.into(),
            name: name.into(),
            slug: name.into(),
            state: SessionState::Paused,
            cwd: PathBuf::from("/home/x/spm"),
            repos: Vec::new(),
            ticket: None,
            port: None,
            started: true,
            last_active: 0,
            last_input: 0,
            recap: None,
            recap_at: 0,
            archived_from: None,
        }
    }

    fn worktree(path: &str, branch: &str) -> SessionRepo {
        SessionRepo {
            name: "api-mate".into(),
            worktree: PathBuf::from(path),
            branch: branch.into(),
        }
    }

    fn run(records: &mut [SessionRecord], command: &Command) -> Reply {
        let mut refs: Vec<&mut SessionRecord> = records.iter_mut().collect();
        apply(&mut refs, command)
    }

    fn port(session: &str, port: Option<u8>) -> Command {
        Command::Port {
            session: session.into(),
            port,
        }
    }

    #[test]
    fn a_session_is_found_by_id_then_slug_then_name() {
        let mut records = vec![record("id-1", "a"), record("id-2", "b")];
        records[1].slug = "b-slug".into();
        for selector in ["id-2", "b-slug", "b"] {
            let reply = run(&mut records, &port(selector, Some(3)));
            assert!(reply.ok, "{selector}: {}", reply.text);
            records[1].port = None;
        }
        let reply = run(&mut records, &port("nobody", Some(3)));
        assert!(!reply.ok);
        assert!(reply.text.contains("nobody"), "{}", reply.text);
    }

    #[test]
    fn a_port_number_above_nine_is_refused() {
        let mut records = vec![record("id-1", "a")];
        let reply = run(&mut records, &port("a", Some(10)));
        assert!(!reply.ok);
        assert!(!reply.changed);
        assert_eq!(records[0].port, None);
    }

    #[test]
    fn a_repeated_port_number_is_accepted_and_reported() {
        let mut records = vec![record("id-1", "a"), record("id-2", "b")];
        records[0].port = Some(3);
        let reply = run(&mut records, &port("b", Some(3)));
        assert!(reply.ok && reply.changed);
        assert_eq!(records[1].port, Some(3));
        assert!(reply.text.contains("a also uses 3"), "{}", reply.text);
    }

    #[test]
    fn an_archived_session_frees_its_port_number() {
        let mut records = vec![record("id-1", "a"), record("id-2", "b")];
        records[0].port = Some(3);
        records[0].state = SessionState::Archived;
        let reply = run(&mut records, &Command::Ports);
        assert!(reply.text.contains("3  free"), "{}", reply.text);
        let reply = run(&mut records, &port("b", Some(3)));
        assert!(!reply.text.contains("also uses"), "{}", reply.text);
    }

    #[test]
    fn the_port_list_names_every_holder_and_every_free_number() {
        let mut records = vec![record("id-1", "a"), record("id-2", "b")];
        records[0].port = Some(1);
        records[1].port = Some(1);
        let text = run(&mut records, &Command::Ports).text;
        assert!(text.contains("1  a, b"), "{text}");
        assert!(
            text.contains("0  free") && text.contains("9  free"),
            "{text}"
        );
        assert_eq!(text.lines().count(), usize::from(PORT_NUMBERS));
    }

    #[test]
    fn clearing_a_port_number_frees_it() {
        let mut records = vec![record("id-1", "a")];
        records[0].port = Some(4);
        let reply = run(&mut records, &port("a", None));
        assert!(reply.changed);
        assert_eq!(records[0].port, None);
    }

    #[test]
    fn a_ticket_is_trimmed_and_an_empty_one_is_refused() {
        let mut records = vec![record("id-1", "a")];
        let set = |t: &str| Command::Ticket {
            session: "a".into(),
            ticket: Some(t.into()),
        };
        assert!(run(&mut records, &set(" CLUB-1138 ")).changed);
        assert_eq!(records[0].ticket.as_deref(), Some("CLUB-1138"));
        let reply = run(&mut records, &set("  "));
        assert!(!reply.ok);
        assert_eq!(records[0].ticket.as_deref(), Some("CLUB-1138"));
    }

    #[test]
    fn a_worktree_registers_once() {
        let mut records = vec![record("id-1", "a")];
        let add = Command::WorktreeAdd {
            session: "a".into(),
            worktree: worktree("/w/api-mate", "feature/x"),
        };
        assert!(run(&mut records, &add).changed);
        let again = run(&mut records, &add);
        assert!(again.ok && !again.changed, "{}", again.text);
        assert_eq!(records[0].repos.len(), 1);
    }

    #[test]
    fn registering_a_worktree_again_refreshes_its_branch() {
        let mut records = vec![record("id-1", "a")];
        records[0]
            .repos
            .push(worktree("/w/api-mate", "feature/old"));
        let reply = run(
            &mut records,
            &Command::WorktreeAdd {
                session: "a".into(),
                worktree: worktree("/w/api-mate", "feature/new"),
            },
        );
        assert!(reply.changed);
        assert_eq!(records[0].repos.len(), 1);
        assert_eq!(records[0].repos[0].branch, "feature/new");
    }

    #[test]
    fn removing_a_worktree_the_session_never_registered_fails() {
        let mut records = vec![record("id-1", "a")];
        let reply = run(
            &mut records,
            &Command::WorktreeRm {
                session: "a".into(),
                path: PathBuf::from("/w/elsewhere"),
            },
        );
        assert!(!reply.ok && !reply.changed);
    }

    #[test]
    fn removing_a_worktree_forgets_it_and_keeps_the_others() {
        let mut records = vec![record("id-1", "a")];
        records[0].repos.push(worktree("/w/api-mate", "x"));
        records[0].repos.push(worktree("/w/crm-mate", "x"));
        let reply = run(
            &mut records,
            &Command::WorktreeRm {
                session: "a".into(),
                path: PathBuf::from("/w/api-mate"),
            },
        );
        assert!(reply.changed);
        assert_eq!(records[0].repos.len(), 1);
        assert_eq!(records[0].repos[0].worktree, PathBuf::from("/w/crm-mate"));
    }

    #[test]
    fn show_prints_the_ticket_port_and_each_worktree() {
        let mut records = vec![record("id-1", "a")];
        records[0].ticket = Some("CLUB-1".into());
        records[0].port = Some(2);
        records[0].repos.push(worktree("/w/portal-mate", ""));
        let text = run(
            &mut records,
            &Command::Show {
                session: "a".into(),
            },
        )
        .text;
        assert!(text.contains("ticket    CLUB-1"), "{text}");
        assert!(text.contains("port      2"), "{text}");
        assert!(text.contains("/w/portal-mate  detached"), "{text}");
    }

    #[test]
    fn the_list_leaves_archived_sessions_out() {
        let mut records = vec![record("id-1", "kept"), record("id-2", "gone")];
        records[1].state = SessionState::Archived;
        let text = run(&mut records, &Command::List).text;
        assert!(text.contains("kept") && !text.contains("gone"), "{text}");
    }

    #[test]
    fn a_command_survives_a_round_trip_through_json() {
        let command = Command::WorktreeAdd {
            session: "a".into(),
            worktree: worktree("/w/api-mate", "x"),
        };
        let text = serde_json::to_string(&command).expect("serializes");
        let back: Command = serde_json::from_str(&text).expect("reads back");
        assert_eq!(back, command);
    }
}
