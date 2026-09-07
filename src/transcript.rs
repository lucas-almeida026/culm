//! Reading a Claude Code transcript. Pure parsing, so every rule here is unit
//! tested. The file itself is read through `Store`.
//!
//! A transcript is one JSON object per line. culm needs two things from it: the name
//! the user gave the session, and the first thing the user asked, which names a
//! session that was never renamed.

use std::path::PathBuf;

use serde_json::Value;

/// The longest prompt culm hands to a namer. A first message can hold a whole file.
const PROMPT_LIMIT: usize = 1500;

/// Tags whose whole block is machinery rather than something the user typed.
const MACHINERY: [&str; 7] = [
    "command-name",
    "command-message",
    "command-args",
    "local-command-caveat",
    "local-command-stdout",
    "ide_selection",
    "system-reminder",
];

/// What culm takes from a transcript.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TranscriptHead {
    pub id: String,
    /// The directory the session ran in. `--resume` needs it, because Claude Code
    /// stores a transcript under the working directory it was started from.
    pub cwd: Option<PathBuf>,
    /// The name the user gave the session inside Claude Code.
    pub custom_title: Option<String>,
    /// The first thing the user actually asked, with the machinery stripped out.
    pub first_prompt: Option<String>,
}

/// Reads the head of one transcript.
///
/// The whole file is walked, because a rename appends a `custom-title` line at the
/// point it happened and the last one wins. A line is parsed only while it can still
/// carry something needed, so a large transcript costs one pass and few parses.
#[must_use]
pub fn scan(id: &str, text: &str) -> TranscriptHead {
    let mut head = TranscriptHead {
        id: id.to_string(),
        ..TranscriptHead::default()
    };
    for line in text.lines() {
        let wants_prompt = head.first_prompt.is_none() || head.cwd.is_none();
        let may_be_title = line.contains("custom-title");
        if !wants_prompt && !may_be_title {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match value.get("type").and_then(Value::as_str) {
            Some("custom-title") => {
                if let Some(title) = value.get("customTitle").and_then(Value::as_str) {
                    head.custom_title = Some(title.to_string());
                }
            }
            Some("user") if wants_prompt => read_user(&value, &mut head),
            _ => {}
        }
    }
    head
}

/// Takes the working directory and, when the line carries one, the first prompt.
///
/// A sidechain line belongs to a subagent, and a meta line is a caveat culm did not
/// write. Neither names the session.
fn read_user(value: &Value, head: &mut TranscriptHead) {
    if value.get("isSidechain").and_then(Value::as_bool) == Some(true) {
        return;
    }
    if head.cwd.is_none()
        && let Some(cwd) = value.get("cwd").and_then(Value::as_str)
    {
        head.cwd = Some(PathBuf::from(cwd));
    }
    if head.first_prompt.is_some() || value.get("isMeta").and_then(Value::as_bool) == Some(true) {
        return;
    }
    let Some(content) = value.pointer("/message/content") else {
        return;
    };
    let raw = match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(" "),
        _ => return,
    };
    let prompt = clean(&raw);
    // A line that is only machinery, such as a slash command or its output, leaves
    // the search running for the first line the user wrote.
    if !prompt.is_empty() {
        head.first_prompt = Some(prompt);
    }
}

/// Removes the machinery blocks and collapses the rest onto one line.
fn clean(text: &str) -> String {
    let mut out = text.to_string();
    for tag in MACHINERY {
        let open = format!("<{tag}>");
        let close = format!("</{tag}>");
        while let Some(start) = out.find(&open) {
            let Some(end) = out[start..].find(&close) else {
                break;
            };
            out.replace_range(start..start + end + close.len(), "");
        }
    }
    let joined = out.split_whitespace().collect::<Vec<_>>().join(" ");
    joined.chars().take(PROMPT_LIMIT).collect()
}

/// True when the file name is `<uuid>.jsonl`.
///
/// A transcript directory also holds a `memory` directory and other files, and only
/// a session file may be imported.
#[must_use]
pub fn is_session_file(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".jsonl") else {
        return false;
    };
    stem.len() == 36
        && stem.matches('-').count() == 4
        && stem.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(content: &str) -> String {
        format!(
            r#"{{"type":"user","isSidechain":false,"cwd":"/home/x/spm","message":{{"role":"user","content":{}}}}}"#,
            serde_json::Value::String(content.to_string())
        )
    }

    #[test]
    fn the_last_custom_title_wins() {
        let text = [
            r#"{"type":"custom-title","customTitle":"first name"}"#,
            &user("do the thing"),
            r#"{"type":"custom-title","customTitle":"second name"}"#,
        ]
        .join("\n");
        let head = scan("id-1", &text);
        assert_eq!(head.custom_title.as_deref(), Some("second name"));
    }

    #[test]
    fn the_first_prompt_comes_from_a_string_or_from_text_blocks() {
        let head = scan("id-1", &user("plain string prompt"));
        assert_eq!(head.first_prompt.as_deref(), Some("plain string prompt"));

        let blocks = r#"{"type":"user","cwd":"/home/x/spm","message":{"content":[{"type":"text","text":"block one"},{"type":"image"},{"type":"text","text":"block two"}]}}"#;
        let head = scan("id-1", blocks);
        assert_eq!(head.first_prompt.as_deref(), Some("block one block two"));
    }

    #[test]
    fn a_sidechain_line_is_not_the_first_prompt() {
        let text = [
            r#"{"type":"user","isSidechain":true,"cwd":"/elsewhere","message":{"content":"a subagent asked this"}}"#,
            &user("the user asked this"),
        ]
        .join("\n");
        let head = scan("id-1", &text);
        assert_eq!(head.first_prompt.as_deref(), Some("the user asked this"));
        assert_eq!(head.cwd, Some(PathBuf::from("/home/x/spm")));
    }

    #[test]
    fn a_meta_line_is_not_the_first_prompt() {
        let text = [
            r#"{"type":"user","isMeta":true,"cwd":"/home/x/spm","message":{"content":"Caveat: generated while running local commands"}}"#,
            &user("the user asked this"),
        ]
        .join("\n");
        let head = scan("id-1", &text);
        assert_eq!(head.first_prompt.as_deref(), Some("the user asked this"));
    }

    #[test]
    fn a_slash_command_line_is_not_the_first_prompt() {
        let text = [
            user("<command-name>/model</command-name><command-args></command-args>"),
            user("the user asked this"),
        ]
        .join("\n");
        let head = scan("id-1", &text);
        assert_eq!(head.first_prompt.as_deref(), Some("the user asked this"));
    }

    #[test]
    fn lines_before_the_first_prompt_are_skipped() {
        let text = [
            r#"{"type":"queue-operation","operation":"add"}"#,
            r#"{"type":"mode","mode":"normal"}"#,
            &user("the user asked this"),
        ]
        .join("\n");
        assert_eq!(
            scan("id-1", &text).first_prompt.as_deref(),
            Some("the user asked this")
        );
    }

    #[test]
    fn an_unparsable_line_is_skipped() {
        let text = ["not json at all", &user("the user asked this")].join("\n");
        assert_eq!(
            scan("id-1", &text).first_prompt.as_deref(),
            Some("the user asked this")
        );
    }

    #[test]
    fn machinery_is_stripped_from_the_prompt() {
        let head = scan(
            "id-1",
            &user("<system-reminder>ignore me</system-reminder>fix\n the bug"),
        );
        assert_eq!(head.first_prompt.as_deref(), Some("fix the bug"));
    }

    #[test]
    fn a_long_prompt_is_cut() {
        let head = scan("id-1", &user(&"a".repeat(5000)));
        assert_eq!(head.first_prompt.map(|p| p.chars().count()), Some(1500));
    }

    #[test]
    fn a_transcript_with_no_prompt_has_no_name_source() {
        let head = scan("id-1", r#"{"type":"mode","mode":"normal"}"#);
        assert_eq!(head.custom_title, None);
        assert_eq!(head.first_prompt, None);
    }

    #[test]
    fn only_a_uuid_file_is_a_session() {
        assert!(is_session_file(
            "0b1a26fb-6e43-4d42-b67d-768dc9256d90.jsonl"
        ));
        assert!(!is_session_file("memory"));
        assert!(!is_session_file("notes.jsonl"));
        assert!(!is_session_file(
            "0b1a26fb-6e43-4d42-b67d-768dc9256d90.json"
        ));
    }
}
