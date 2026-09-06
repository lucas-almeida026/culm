//! Attention markers. A Claude Code hook posts one payload to a Unix socket that the
//! running interface owns, and the interface marks the matching session.
//!
//! The state machine and the settings merge are pure, so both are unit tested. Only
//! `listen` and `send_from_stdin` touch the operating system.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{Value, json};

/// The hook events culm installs. `PermissionRequest` reports a prompt, `Stop`
/// reports the end of a turn, and the rest report that the session moved on.
pub const EVENTS: [&str; 9] = [
    "PermissionRequest",
    "Notification",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "Stop",
    "SubagentStop",
    "UserPromptSubmit",
    "SessionEnd",
];

/// What a session needs from the user. The order is the priority order, so a
/// derived `Ord` compares markers correctly.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Attention {
    #[default]
    None,
    Done,
    NeedsAnswer,
    NeedsPermission,
}

impl Attention {
    /// The sidebar prefix. Two spaces when nothing is pending, so names stay aligned.
    #[must_use]
    pub fn emoji(self) -> &'static str {
        match self {
            Attention::None => "  ",
            Attention::Done => "✅",
            Attention::NeedsAnswer => "❓",
            Attention::NeedsPermission => "🔐",
        }
    }

    /// The next marker after one hook event.
    ///
    /// A marker never clears because the user focused the session. A marker clears
    /// when a later event proves the session moved on.
    ///
    /// No event reports that the user answered a permission prompt, so a permission
    /// marker also clears on the keystroke that answers it. `App::send_to_focus`
    /// holds that half of the rule.
    ///
    /// A done marker retires only on `UserPromptSubmit` or `SessionEnd`, because a
    /// background tool or subagent can report in after `Stop`.
    #[must_use]
    pub fn apply(self, event: &HookEvent) -> Self {
        let asking = event.tool_name.as_deref() == Some("AskUserQuestion");
        match event.hook_event_name.as_str() {
            "PermissionRequest" => Attention::NeedsPermission,
            "Notification" => match event.notification_type.as_deref() {
                Some("permission_prompt") => Attention::NeedsPermission,
                // `max` keeps a permission marker above an answer marker.
                Some("elicitation_dialog" | "elicitation_url_dialog" | "agent_needs_input") => {
                    self.max(Attention::NeedsAnswer)
                }
                // `idle_prompt` fires after about a minute of silence, and says only
                // that the session is idle, which `Stop` already reported. The rest
                // (`auth_success`, `agent_completed`, `quota_*`, an unknown kind, or a
                // payload with no kind at all) want nothing from the user.
                _ => self,
            },
            "PreToolUse" if asking => Attention::NeedsAnswer,
            // A tool or a subagent that reports in after the turn ended must not wipe
            // the done marker. Every hook of one event runs in parallel, and the
            // `culm hook` processes race to the socket, so these arrive out of order.
            "PreToolUse" | "PostToolUse" | "PostToolUseFailure" | "SubagentStop"
                if self == Attention::Done =>
            {
                self
            }
            "PreToolUse" | "PostToolUse" | "PostToolUseFailure" | "SubagentStop" => Attention::None,
            "Stop" => Attention::Done,
            // Only the user moving on retires a done marker.
            "UserPromptSubmit" | "SessionEnd" => Attention::None,
            _ => self,
        }
    }
}

/// One hook payload. Claude Code writes the payload to the hook process on stdin.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct HookEvent {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub hook_event_name: String,
    #[serde(default)]
    pub tool_name: Option<String>,
    /// Which kind of notification this is. Only a few kinds want the user.
    #[serde(default)]
    pub notification_type: Option<String>,
}

/// True when `command` is a culm hook entry, whichever path the binary sits at.
#[must_use]
fn is_culm_command(command: &str) -> bool {
    let mut parts = command.split_whitespace();
    let Some(exe) = parts.next() else {
        return false;
    };
    let is_culm = Path::new(exe)
        .file_name()
        .is_some_and(|n| n == "culm" || n == "culm.exe");
    is_culm && parts.next() == Some("hook") && parts.next().is_none()
}

#[must_use]
pub fn hook_command(exe: &Path) -> String {
    format!("{} hook", exe.display())
}

/// True when the settings already route every event to this binary.
#[must_use]
pub fn is_installed(settings: &Value) -> bool {
    EVENTS.iter().all(|event| {
        settings["hooks"][event]
            .as_array()
            .is_some_and(|groups| groups.iter().any(group_holds_culm))
    })
}

fn group_holds_culm(group: &Value) -> bool {
    group["hooks"].as_array().is_some_and(|hooks| {
        hooks
            .iter()
            .any(|h| h["command"].as_str().is_some_and(is_culm_command))
    })
}

/// Adds a culm entry to every event, and keeps every entry culm did not write.
///
/// Removing first makes the merge idempotent, and moves the entry when the binary
/// moved.
#[must_use]
pub fn install(settings: &Value, exe: &Path) -> Value {
    let mut out = uninstall(settings);
    let command = hook_command(exe);
    let hooks = out
        .as_object_mut()
        .map(|o| o.entry("hooks").or_insert_with(|| json!({})));
    let Some(hooks) = hooks else {
        return out;
    };
    if !hooks.is_object() {
        *hooks = json!({});
    }
    for event in EVENTS {
        let entry = json!({ "hooks": [{ "type": "command", "command": command }] });
        match hooks.get_mut(event).and_then(Value::as_array_mut) {
            Some(groups) => groups.push(entry),
            None => {
                if let Some(map) = hooks.as_object_mut() {
                    map.insert(event.to_string(), json!([entry]));
                }
            }
        }
    }
    out
}

/// Removes every culm entry and leaves every other entry untouched.
#[must_use]
pub fn uninstall(settings: &Value) -> Value {
    let mut out = settings.clone();
    let Some(hooks) = out.get_mut("hooks").and_then(Value::as_object_mut) else {
        return out;
    };
    for value in hooks.values_mut() {
        let Some(groups) = value.as_array_mut() else {
            continue;
        };
        for group in groups.iter_mut() {
            if let Some(list) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                list.retain(|h| !h["command"].as_str().is_some_and(is_culm_command));
            }
        }
        groups.retain(|g| g["hooks"].as_array().is_none_or(|list| !list.is_empty()));
    }
    hooks.retain(|_, v| v.as_array().is_none_or(|a| !a.is_empty()));
    if hooks.is_empty() {
        out.as_object_mut().map(|o| o.remove("hooks"));
    }
    out
}

/// Where the interface listens for one project.
#[must_use]
pub fn socket_path(slug: &str) -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir).join(format!("culm-{slug}.sock")),
        _ => {
            let uid = nix::unistd::getuid().as_raw();
            PathBuf::from(format!("/tmp/culm-{uid}-{slug}.sock"))
        }
    }
}

/// Binds the socket and returns the channel that carries hook payloads.
///
/// A stale socket file from an earlier run is removed first, because a crash leaves
/// the file behind and `bind` refuses an existing path.
pub fn listen(path: &Path) -> Result<Receiver<HookEvent>> {
    if path.exists() {
        let _ = std::fs::remove_file(path);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    let listener = UnixListener::bind(path).with_context(|| format!("bind {}", path.display()))?;
    let (tx, rx) = mpsc::channel();
    let log = std::env::var_os("CULM_HOOK_LOG").map(PathBuf::from);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut payload = String::new();
            if stream.read_to_string(&mut payload).is_err() {
                continue;
            }
            if let Ok(event) = serde_json::from_str::<HookEvent>(&payload) {
                if let Some(log) = log.as_deref() {
                    record(log, &event);
                }
                if tx.send(event).is_err() {
                    break;
                }
            }
        }
    });
    Ok(rx)
}

/// Appends one line per payload when `CULM_HOOK_LOG` names a file.
///
/// A diagnostic for reading which event follows `Stop`. Nothing in the interface
/// depends on it, and a failure to write is ignored.
fn record(path: &Path, event: &HookEvent) {
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    else {
        return;
    };
    let _ = writeln!(
        file,
        "{} {} {}",
        event.hook_event_name,
        event.tool_name.as_deref().unwrap_or("-"),
        event.notification_type.as_deref().unwrap_or("-")
    );
}

/// The `culm hook` subcommand. Reads the payload on stdin and posts it.
///
/// Never fails the hook. A Claude Code session outside culm has no `CULM_SOCKET` and
/// returns at once.
pub fn send_from_stdin() {
    let Some(path) = std::env::var_os("CULM_SOCKET") else {
        return;
    };
    let mut payload = String::new();
    if std::io::stdin().read_to_string(&mut payload).is_err() {
        return;
    }
    if let Ok(mut stream) = UnixStream::connect(path) {
        let _ = stream.set_write_timeout(Some(Duration::from_millis(200)));
        let _ = stream.write_all(payload.as_bytes());
        let _ = stream.flush();
    }
}

#[cfg(test)]
mod tests {
    // Test code may use `expect` with a message. Library code may not.
    #![allow(clippy::expect_used)]

    use super::*;

    fn event(name: &str) -> HookEvent {
        HookEvent {
            session_id: "s1".into(),
            hook_event_name: name.into(),
            ..Default::default()
        }
    }

    fn notification(kind: &str) -> HookEvent {
        HookEvent {
            notification_type: Some(kind.into()),
            ..event("Notification")
        }
    }

    fn tool_event(name: &str, tool: &str) -> HookEvent {
        HookEvent {
            tool_name: Some(tool.into()),
            ..event(name)
        }
    }

    #[test]
    fn attention_priority_orders_permission_over_answer_over_done() {
        assert!(Attention::NeedsPermission > Attention::NeedsAnswer);
        assert!(Attention::NeedsAnswer > Attention::Done);
        assert!(Attention::Done > Attention::None);
    }

    #[test]
    fn a_permission_request_raises_the_permission_marker() {
        assert_eq!(
            Attention::None.apply(&event("PermissionRequest")),
            Attention::NeedsPermission
        );
    }

    #[test]
    fn a_notification_never_lowers_a_permission_marker() {
        for kind in ["elicitation_dialog", "agent_needs_input", "idle_prompt"] {
            assert_eq!(
                Attention::NeedsPermission.apply(&notification(kind)),
                Attention::NeedsPermission,
                "{kind} must not replace a permission prompt"
            );
        }
    }

    #[test]
    fn an_idle_notification_moves_no_marker() {
        assert_eq!(
            Attention::None.apply(&notification("idle_prompt")),
            Attention::None,
            "a minute of silence is not a question"
        );
        assert_eq!(
            Attention::Done.apply(&notification("idle_prompt")),
            Attention::Done
        );
    }

    #[test]
    fn a_permission_notification_raises_the_permission_marker() {
        assert_eq!(
            Attention::None.apply(&notification("permission_prompt")),
            Attention::NeedsPermission
        );
    }

    #[test]
    fn an_elicitation_notification_raises_the_answer_marker() {
        for kind in [
            "elicitation_dialog",
            "elicitation_url_dialog",
            "agent_needs_input",
        ] {
            assert_eq!(
                Attention::None.apply(&notification(kind)),
                Attention::NeedsAnswer,
                "{kind} wants the user"
            );
        }
    }

    #[test]
    fn an_unknown_notification_moves_no_marker() {
        for kind in [
            "auth_success",
            "agent_completed",
            "quota_exceeded",
            "something_new",
        ] {
            assert_eq!(
                Attention::Done.apply(&notification(kind)),
                Attention::Done,
                "{kind} wants nothing from the user"
            );
        }
        assert_eq!(
            Attention::None.apply(&event("Notification")),
            Attention::None,
            "a payload with no kind moves nothing"
        );
    }

    #[test]
    fn a_tool_call_clears_a_permission_marker() {
        assert_eq!(
            Attention::NeedsPermission.apply(&event("PreToolUse")),
            Attention::None
        );
    }

    #[test]
    fn a_question_raises_the_answer_marker_and_the_reply_clears_it() {
        let asked = Attention::None.apply(&tool_event("PreToolUse", "AskUserQuestion"));
        assert_eq!(asked, Attention::NeedsAnswer);
        assert_eq!(
            asked.apply(&tool_event("PostToolUse", "AskUserQuestion")),
            Attention::None
        );
    }

    #[test]
    fn the_end_of_a_turn_marks_the_session_done_and_a_prompt_clears_it() {
        let done = Attention::None.apply(&event("Stop"));
        assert_eq!(done, Attention::Done);
        assert_eq!(done.apply(&event("UserPromptSubmit")), Attention::None);
    }

    #[test]
    fn a_failed_tool_clears_a_stale_marker() {
        assert_eq!(
            Attention::NeedsPermission.apply(&event("PostToolUseFailure")),
            Attention::None
        );
    }

    #[test]
    fn a_subagent_that_ends_leaves_the_session_not_done() {
        assert_eq!(
            Attention::NeedsAnswer.apply(&event("SubagentStop")),
            Attention::None,
            "the turn is still running, so the session is not done"
        );
    }

    /// Guards against an event added to `EVENTS` and forgotten in `apply`, where it
    /// would fall through to the catch-all and never move a marker.
    ///
    /// A notification only means something with a kind, so the guard supplies one.
    #[test]
    fn every_installed_event_moves_at_least_one_marker() {
        let states = [
            Attention::None,
            Attention::Done,
            Attention::NeedsAnswer,
            Attention::NeedsPermission,
        ];
        for name in EVENTS {
            let payload = if name == "Notification" {
                notification("permission_prompt")
            } else {
                event(name)
            };
            assert!(
                states.iter().any(|s| s.apply(&payload) != *s),
                "{name} is installed but never changes a marker"
            );
        }
    }

    #[test]
    fn a_tool_event_after_stop_leaves_the_session_done() {
        for name in [
            "PreToolUse",
            "PostToolUse",
            "PostToolUseFailure",
            "SubagentStop",
        ] {
            assert_eq!(
                Attention::Done.apply(&event(name)),
                Attention::Done,
                "{name} arriving after Stop must not wipe the done marker"
            );
        }
    }

    #[test]
    fn only_the_user_moving_on_retires_a_done_marker() {
        assert_eq!(
            Attention::Done.apply(&event("UserPromptSubmit")),
            Attention::None
        );
        assert_eq!(Attention::Done.apply(&event("SessionEnd")), Attention::None);
    }

    #[test]
    fn a_permission_request_still_overrides_a_done_marker() {
        assert_eq!(
            Attention::Done.apply(&event("PermissionRequest")),
            Attention::NeedsPermission
        );
    }

    #[test]
    fn an_unknown_event_leaves_the_marker_alone() {
        assert_eq!(
            Attention::NeedsPermission.apply(&event("SessionStart")),
            Attention::NeedsPermission
        );
    }

    #[test]
    fn install_keeps_existing_hooks_and_is_idempotent() {
        let existing = json!({
            "model": "opus",
            "hooks": {
                "Stop": [{ "hooks": [{ "type": "command", "command": "cc-attn stop" }] }]
            }
        });
        let once = install(&existing, Path::new("/usr/bin/culm"));
        let twice = install(&once, Path::new("/usr/bin/culm"));
        assert_eq!(once, twice);
        assert_eq!(once["model"], "opus");
        assert!(is_installed(&once));

        let stop = once["hooks"]["Stop"].as_array().expect("Stop is an array");
        assert_eq!(stop.len(), 2, "the existing entry survives");
        assert_eq!(stop[0]["hooks"][0]["command"], "cc-attn stop");
    }

    #[test]
    fn install_moves_the_entry_when_the_binary_moved() {
        let first = install(&json!({}), Path::new("/old/culm"));
        let second = install(&first, Path::new("/new/culm"));
        let stop = second["hooks"]["Stop"]
            .as_array()
            .expect("Stop is an array");
        assert_eq!(stop.len(), 1);
        assert_eq!(stop[0]["hooks"][0]["command"], "/new/culm hook");
    }

    #[test]
    fn uninstall_removes_only_culm_entries() {
        let existing = json!({
            "hooks": {
                "Stop": [{ "hooks": [{ "type": "command", "command": "cc-attn stop" }] }]
            }
        });
        let installed = install(&existing, Path::new("/usr/bin/culm"));
        let cleaned = uninstall(&installed);
        assert_eq!(cleaned, existing);
        assert!(!is_installed(&cleaned));
    }

    #[test]
    fn uninstall_leaves_no_empty_hooks_object_behind() {
        let installed = install(&json!({ "model": "opus" }), Path::new("/usr/bin/culm"));
        let cleaned = uninstall(&installed);
        assert_eq!(cleaned, json!({ "model": "opus" }));
    }

    #[test]
    fn a_hook_command_is_recognized_by_its_binary_name() {
        assert!(is_culm_command("/usr/local/bin/culm hook"));
        assert!(is_culm_command("culm hook"));
        assert!(!is_culm_command("cc-attn stop"));
        assert!(!is_culm_command("culm open spm"));
        assert!(!is_culm_command("notculm hook"));
    }
}
