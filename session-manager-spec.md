# Session manager — spec

Requirements for culm, a tool that manages many Claude Code sessions across several repositories. Written 2026-09-06.

The comparison that produced the first set of decisions lives in `tmux-claude-setup.md`. That file is not part of this repository. Evidence for the stack lives in [FINDINGS.md](FINDINGS.md).

## Purpose

Run many Claude Code sessions at the same time, one per feature. Group the sessions by project. Return to any session later with the whole conversation intact. Show which session needs attention, and why.

## Concepts

| Term | Definition |
| --- | --- |
| Project | A managed directory. Any directory qualifies. A project needs no git repository at its root. |
| Repository | A git repository that belongs to a project. A repository lives inside the project directory or outside the project directory. |
| Session | One Claude Code process, with one transcript, one name, one session id, and one attention state. |
| Active session | A session whose process runs. |
| Paused session | A session with no process. culm holds the session id and starts the session on demand. |
| Worktree | A git worktree plus a branch, created for one session in one repository. |
| Registry | One global file that lists every project. |

## Decisions

| Dimension | Decision |
| --- | --- |
| Runs in a non-git directory | Yes. culm manages projects itself and accepts any directory as a project root. |
| Unit of organization | The project. One project per managed directory. |
| Cross-repository work in one session | Yes. A session reaches every repository in its project. culm adds and removes repositories, including repositories outside the project root. |
| Parallel edits to the same repository | Yes, through git worktrees and one branch per session. |
| Project registry | Global. culm opens any project from any working directory. |
| Session identity | culm generates a UUID and passes `--session-id` on first start. |
| Resume an existing transcript | Yes. culm runs `claude --resume <uuid>`, which reloads the whole transcript. |
| Resume policy | Split. Active sessions start when the project opens. A paused session starts when the user selects the session. |
| Pause | SIGTERM to the child. culm records the session id. |
| Active session limit | Nine per project, because one digit addresses a session. |
| Migrated history | Out of scope. The 2026-09-02 path migration was a one-time task. |
| Transcript storage | Native. culm never changes where the Claude Code CLI writes transcripts. |
| Attention signal source | Harness hooks, installed globally, delivered over a Unix socket. |
| Attention granularity | Three states: needs permission, needs answer, done. Priority-ordered, emoji-coded, and color-coded. |
| Marker clears when | The user resolves the issue through interaction. |
| Non-code sessions | Native, with no difference in handling. |
| Session naming | Manual, with no default pattern. A rename applies to an existing session. |
| Key leader | `Alt`. culm reserves no function key. |
| Key configuration | One TOML file. Every binding is replaceable. |
| Project removal | Deletes the culm state, the Claude Code data, and the worktrees of a project. Never deletes a branch or a source file. |
| Git diff, commit, and push | Out of scope. |
| Auto-accept prompts | Out of scope, by choice. |
| Model and effort per session | Yes. Each session is a real terminal process that runs the Claude Code CLI, and culm relays Claude commands to that terminal. |
| Reboot survival | Saved state plus `--resume`. A reboot ends every process, so no supervisor changes the mechanism. |
| Crash survival | None. An unexpected exit of the interface ends every session. Deferred, not refused. |
| Session rendering | Embedded. culm renders the focused session as a full interactive terminal panel. One session is visible at a time. Every active session runs whether or not the session is visible. |
| Interface surface | A terminal application, plus a command line for every project operation. |
| Footprint | One Rust binary plus configuration files. The stack is ratatui, portable-pty, and a vt100 screen. |
| External dependencies | `git` for worktrees, and the Claude Code CLI. No tmux. No jq. No gh. |
| Terminal support | Any Linux terminal. The kitty keyboard protocol is an enhancement, never a requirement. |
| Process supervision | One process. culm owns every pseudoterminal. No daemon, and no external supervisor. |

## Requirements

### Projects

1. Register a project from any directory. Do not require a git repository at the project root.
2. Add a repository to a project. Accept a path inside the project root, and a path outside the project root.
3. Remove a repository from a project. Do not delete the repository.
4. List projects. Open one project at a time in the interface.
5. Hold the project list in one global registry, so that culm opens any project from any working directory.

### Command line

6. Provide every project operation as a command, so that a shell script reaches culm without the interface.
7. Accept a project id or a project slug wherever a command takes a project.
8. Open the interface with `culm open <project>`.
9. With no argument, open the project that owns the working directory. With no such project, show the project list.
10. Remove a project with `culm project rm <project>`.
11. Delete the registry entry, the culm state of the project, and the Claude Code data of the project root. Require a typed confirmation.
12. Skip the confirmation under `--force`.
13. Delete the Claude Code data of every path under the project root under `--recursive`. Include the transcript, the tool records, and every other file of the session.
14. Delete every worktree of the project under `--recursive`.
15. Resolve one Claude Code directory per repository path. A project that holds many repositories holds many such directories.
16. Delete no branch, no source file, and no repository under any flag. A worktree directory is the only working copy that `--recursive` removes.
17. Name every worktree that holds an uncommitted change in the confirmation prompt. `--force` skips the prompt and removes the worktree anyway.

### Sessions

18. Create a session inside a project. Take the name from the user. Apply no default pattern.
19. Rename a session after creation.
20. Run each session as a real terminal process that runs the Claude Code CLI. Claude commands, model selection, and effort selection then reach the process without translation.
21. Generate a UUID for a new session. Pass the UUID as `--session-id` on first start.
22. Resume a session with `claude --resume <uuid>`. The Claude Code CLI accepts `--session-id <uuid>` and `-r, --resume [value]`, checked on 2026-09-06 against the CLI version recorded in [FINDINGS.md](FINDINGS.md).
23. Start a session in the worktree of its first repository. Start a session with no repository at the project root.
24. Pass `--add-dir <project root>`, so that the session reaches every repository of the project.
25. Pass `--append-system-prompt` with the repository to worktree mapping. The mapping is convention only. Nothing enforces it, and a model decides whether to follow it.
26. Attach to a session and detach from a session without ending the process.
27. Archive a session. Archive is the default action of the Claude Code CLI. The mechanism that marks a Claude Code session archived is unverified.
28. Delete a session on request. Deletion removes the transcript from disk. Deletion has no undo, so require a typed confirmation.
29. Refuse to delete a session whose process runs. Report that the session needs a pause first.
30. Delete a Claude Code session that no project owns.

### Session state

31. Give every session one of two states: active or paused.
32. Split the session list into two halves. The first half holds active sessions. The second half holds paused sessions.
33. Start every active session when the project opens.
34. Start a paused session when the user focuses the row and asks for the resume. Start no process before that.
35. Focus a row on a click. A click alone never starts a process.
36. Pause a session with SIGTERM. An in-flight tool call is lost. Do not wait for an idle session.
37. Move a session to the other half when the state of the session changes.

### Parallel edits

38. Create a git worktree and a branch for a session that edits a repository, so that two sessions editing one repository do not collide.
39. Create every worktree under `<project root>/.worktrees/`.
40. Name each worktree directory `<repository name>-<session slug>`. A repository outside the project root then keeps a distinct directory.
41. Use one branch name per session. Share the branch name across every repository that the session edits.
42. Take the repository list of a session from the user at creation. Add a repository to an existing session on request.
43. Run without worktrees when a project holds no git repository. Report the state and continue.
44. Place no limit on concurrent sessions in a project without a git repository. Concurrent edits to one file are the responsibility of the user.

### Attention markers

45. Set the marker from Claude Code hooks, not from terminal output.
46. Install the hook entries in `~/.claude/settings.json`. Remove the entries on uninstall.
47. Inject `CULM_SOCKET` into the environment of every session that culm spawns. A hook process inherits the environment, verified on 2026-09-02.
48. Exit the hook when `CULM_SOCKET` is absent, so that a Claude Code session outside culm costs one process start.
49. Read the session id from the hook payload. The payload field that carries the session id is unverified.
50. Deliver the marker over a Unix socket.
51. Support three states with a fixed priority: needs permission, then needs answer, then done.
52. Give each state an emoji and a color.
53. Clear a marker only when the user resolves the issue. Do not clear a marker when the user focuses the session.
54. Clear a permission marker when the next hook for that session arrives. A declined permission fires no terminating hook, verified on 2026-09-02, so no other signal reports the decline.

The culm process owns the socket. A session runs only while culm runs, so a hook fires only while the socket exists. An unexpected exit of culm loses a marker in flight, and ends the session that produced the marker.

### Interface

The model for the interface is a micro-frontend host. Each session behaves as a terminal that runs on its own. culm arranges those terminals and adds management on top.

55. Render each session as a terminal panel inside the interface.
56. Keep the focused panel interactive. Keyboard input reaches the Claude Code process in the focused panel.
57. Render the whole terminal for the focused panel, not a summary and not a snapshot.
58. Show one session at a time, in the manner of a browser with vertical tabs. A session that is not visible keeps running.
59. Show the attention state of a session on the session list, without stealing focus.
60. Forward a paste to the focused session as one bracketed block.
61. Let the user copy text out of the focused panel. Mouse capture removes text selection in some terminals, so provide a binding that turns mouse capture off. In kitty, shift plus drag still selects, verified on 2026-09-06. No other terminal is verified.
62. Show an empty state. With no project registered, show the command that registers a project. With no session in the open project, show the binding that creates a session.
63. Run on a terminal that reports no keyboard enhancement. Show the active keyboard mode in the sidebar.

### Keys

Every reserved key stops belonging to the Claude Code process. Keep the reserved set small.

64. Use `Alt` as the only leader. Reserve no function key.
65. Reserve `Ctrl+q` for quit.
66. Focus a session with `Alt` plus a digit from 1 to 9.
67. Hold at most nine active sessions in a project. Refuse a tenth, and report the limit.
68. Create a session with `Alt+Shift+N`. A new session starts active.
69. Swap the focused session with position N under `Alt+Shift` plus a digit from 1 to 9.
70. Swap with the last position when position N holds no session. With three sessions and position 1 focused, `Alt+Shift+9` swaps position 1 and position 3.
71. Pause the focused active session, and resume the focused paused session, with `Alt+Shift+P`.
72. Toggle nerd mode with `Alt+Shift+D`.
73. Address the active half with a digit. Reach a paused session with a mouse click.
74. Match a shifted digit on the legacy path. `Alt+Shift+1` arrives as `ESC` and `!`. The event carries no digit and no shift modifier. The character depends on the keyboard layout, so the configuration file holds the mapping.

### Configuration

75. Read one configuration file in TOML form from `~/.config/culm/config.toml`.
76. Hold every key binding in the configuration file. Ship the defaults in the binary.
77. Override one binding without restating the rest.
78. Report an unknown action name and an unparsable binding at load. Name the file and the line. Do not ignore the entry.
79. Read the nerd mode default from the same file.

### Statistics

80. Show the resident memory of each session on its sidebar row. Read `VmRSS` from `/proc/<pid>/status` and sum the process tree of the session.
81. Sample memory once per second. Render the last sample.
82. Show the frame rate in the top right corner under nerd mode. Keep nerd mode off by default.
83. Reach `/proc` through a trait, so that a test supplies a fake.

One Claude Code process measured about 436 MB of resident memory on 2026-09-02. Memory, and not render cost, is the limit on the session count.

### Persistence

84. On closing a project, pause every session and record which sessions were active.
85. On opening a project, restore both halves of the session list to the recorded state.
86. Survive a reboot through the saved state, because a reboot ends every terminal process.

## Out of scope

- Git diff, commit, push, and any other version control action beyond worktree and branch management.
- Automatic acceptance of permission prompts.
- Any change to where the Claude Code CLI writes transcripts.
- Migration of transcripts between project directories.
- Any target other than Linux.

## Resolved questions

Every question below was open in the first version of this document. Each row records the resolution and the date of the resolution.

| # | Question | Resolution | Date |
| --- | --- | --- | --- |
| 1 | Eager or lazy resume | Split. An active session starts when the project opens. A paused session starts on selection. | 2026-09-06 |
| 2 | Worktrees for a cross-repository session | The user names the repositories of a session. One branch name per session, shared across those repositories. | 2026-09-06 |
| 3 | Meaning of pause | SIGTERM, with no wait for an idle session. An in-flight tool call is lost. | 2026-09-06 |
| 4 | Hook installation | Global entries in `~/.claude/settings.json`. The hook exits when `CULM_SOCKET` is absent. | 2026-09-06 |
| 5 | A declined permission fires no terminating hook | The marker persists until the next hook for that session arrives. culm polls nothing. | 2026-09-06 |
| 6 | The jq dependency | Dropped. The binary parses the hook payload itself. | 2026-09-06 |
| 7 | Deleting a session that is running | Refuse. The user pauses the session first. | 2026-09-06 |
| 8 | Interface surface | A terminal application built on ratatui, portable-pty, and a vt100 screen, plus a command line. FINDINGS.md holds the measurements. | 2026-09-06 |
| 9 | Panel size under two attached clients | Moot. culm owns each pseudoterminal, so no second client shares the view. | 2026-09-06 |
| 10 | Inactive panels | Parse every session. Draw the focused session only. A background flood drained 12.8 GB while the visible panel rendered at p50 0.14 ms. | 2026-09-06 |
| 11 | Keyboard fidelity | `shift+tab`, `Ctrl` plus a letter, `Esc`, arrows, and a bracketed paste reach the child unchanged on the legacy path. The kitty protocol path is unverified. | 2026-09-06 |
| 12 | Session identity for a hook | culm generates the UUID and passes `--session-id`. `$TMUX_PANE` is gone with tmux. | 2026-09-06 |
| 13 | Process supervision | One process. culm owns every pseudoterminal. No daemon, and no external supervisor. | 2026-09-06 |

## Deferred

Crash survival. culm holds the master side of each pseudoterminal. When culm exits, the master closes, and the kernel hangs up each child. An unexpected exit of the interface therefore ends every session in the project.

The loss is bounded. The Claude Code CLI writes the transcript during the run, so `--resume` recovers every completed turn. An unexpected exit loses the turn in flight, and costs one process start plus one transcript reload for each active session.

Two designs remove the loss. A culm daemon holds every master and the interface attaches over a socket. One `abduco` process per session holds one master each, and culm attaches as a client. Both changes land behind `PtySpawner` in `src/pty.rs`, which is the only place culm starts a process.

Build neither until the loss becomes real. Order of work is to make it work first.

## Open questions

None stand on 2026-09-06.
