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
| Shell | A plain terminal at position 0. Not a Claude Code session. It carries no session id, no transcript, and no attention marker, and culm never saves it. |
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
4. List the repositories of a project, with the path each one is checked out at.
5. List projects. Open one project at a time in the interface.
6. Hold the project list in one global registry, so that culm opens any project from any working directory.

### Command line

7. Provide every project operation as a command, so that a shell script reaches culm without the interface.
8. Accept the project name wherever a command takes a project. A project has no other identifier.
9. Open the interface with `culm open <project>`.
10. With no argument, open the project that owns the working directory. With no such project, report that and name the command that registers one.
11. Name a project with `--name <name>` on `culm project new`. Without the flag, take the name from the last part of the path.
12. Rename a project with `culm project alter name <name>`. `culm open <name>` takes the new name from then on.
13. Reduce a project name to a slug, because the name is the identifier a command takes and the name of the state file.
14. Refuse a project name another project already holds, on both commands. A name the user typed is a name the user means to type again, so never number it.
15. Move the saved state of a project to the new name on a rename. Keep the root, the repositories, and every session.
16. Remove a project with `culm project rm <project>`.
17. Delete the registry entry, the culm state of the project, and the Claude Code data of the project root. Require a typed confirmation.
18. Skip the confirmation under `--force`.
19. Delete the Claude Code data of every path under the project root under `--recursive`. Include the transcript, the tool records, and every other file of the session.
20. Delete every worktree of the project under `--recursive`.
21. Resolve one Claude Code directory per repository path. A project that holds many repositories holds many such directories.
22. Delete no branch, no source file, and no repository under any flag. A worktree directory is the only working copy that `--recursive` removes.
23. Name every worktree that holds an uncommitted change in the confirmation prompt. `--force` skips the prompt and removes the worktree anyway.
24. Import every Claude Code session found under the project root with `culm project import`, with `culm project new <path> --import-native-sessions`, and with `culm open --import-native-sessions`.
25. Keep the session id of an imported session, so that `--resume` reaches the same conversation.
26. Import a session as paused. Start no process during an import.
27. Name an imported session from the title the user gave it inside Claude Code, slugified. With no title, ask a headless Claude Code child on the smallest model to name the session from the first prompt of the transcript. With no answer, name the session from its id.
28. Start an imported session in the working directory recorded in the transcript, because the Claude Code CLI reads a transcript from the directory the session was started in. Skip a transcript that records no working directory, and report it.
29. Import no session the project already holds. A second import adds only the transcripts that appeared since the first.

### Sessions

30. Create a session inside a project. Take the name from the user. Apply no default pattern.
31. Rename a session after creation, active or paused. Change the displayed name only. The slug, the branch, and every worktree path keep the names they were created with, because a running session holds those paths open.
32. Run each session as a real terminal process that runs the Claude Code CLI. Claude commands, model selection, and effort selection then reach the process without translation.
33. Generate a UUID for a new session. Pass the UUID as `--session-id` on first start.
34. Resume a session with `claude --resume <uuid>`. The Claude Code CLI accepts `--session-id <uuid>` and `-r, --resume [value]`, checked on 2026-09-06 against the CLI version recorded in [FINDINGS.md](FINDINGS.md).
35. Start a session in the worktree of its first repository. Start a session with no repository at the project root.
36. Pass `--add-dir <project root>`, so that the session reaches every repository of the project.
37. Pass `--append-system-prompt` with the repository to worktree mapping. The mapping is convention only. Nothing enforces it, and a model decides whether to follow it.
38. Attach to a session and detach from a session without ending the process.
39. Delete a session on request. Deletion removes the transcript from disk. Deletion has no undo, so ask for a confirmation before it runs.
40. Answer the delete confirmation with a yes button and a no button. Start the cursor on no, so that a stray `Enter` never deletes.
41. Move the cursor between the buttons with `y`, with `n`, with an arrow, and with `Tab`. A letter moves the cursor and never answers on its own, so a delete always takes two keys.
42. Answer the confirmation with `Enter` on the button under the cursor, or with a click on a button. Cancel it with `Esc`.
43. Refuse to delete a session whose process runs. Report that the session needs a pause first.

### Session state

44. Give every session one of two states: active or paused.
45. Split the session list into two halves. The first half holds active sessions. The second half holds paused sessions.
46. Start every active session when the project opens.
47. Start a paused session when the user focuses the row and asks for the resume. Start no process before that.
48. Focus a row on a click. A click alone never starts a process.
49. Pause a session with SIGTERM. An in-flight tool call is lost. Do not wait for an idle session.
50. Move a session to the other half when the state of the session changes.
51. Order the paused half by the time of the last interaction, newest first. Keep the active half in its own order, because a digit addresses it.
52. Record the time of the last interaction when a session is created, resumed, paused, and sent a keystroke. Write the record on the next tick, so that typing never costs a file write.

### The shell

Position 0 holds a plain terminal so that the user runs an ordinary command without leaving culm.

53. Open one shell per project, at the project root, focused when the project opens.
54. Keep position 0 filled. Start a new shell when the user ends the one that is there.
55. Give the shell no `CULM_SOCKET`, so a Claude Code session the user starts by hand inside it posts no marker culm cannot place.
56. Read nothing from the shell. A `cd` inside it changes no session and no project.
57. Exempt the shell from pause, resume, delete, markers, worktrees, and saved state.

### Parallel edits

58. Create a git worktree and a branch for a session that edits a repository, so that two sessions editing one repository do not collide.
59. Create every worktree under `<project root>/.worktrees/`.
60. Name each worktree directory `<repository name>-<session slug>`. A repository outside the project root then keeps a distinct directory.
61. Use one branch name per session. Share the branch name across every repository that the session edits.
62. Take the repository list of a session from the user at creation. A repository the user adds later is the user's own work, through the shell at position 0.
63. Run without worktrees when a project holds no git repository. Report the state and continue.
64. Place no limit on concurrent sessions in a project without a git repository. Concurrent edits to one file are the responsibility of the user.

### Attention markers

65. Set the marker from Claude Code hooks, not from terminal output.
66. Install the hook entries in `~/.claude/settings.json`. Remove the entries on uninstall.
67. Install `PermissionRequest`, `Notification`, `PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `Stop`, `SubagentStop`, `UserPromptSubmit`, and `SessionEnd`. A failed tool and an ended subagent otherwise leave a stale marker.
68. Inject `CULM_SOCKET` into the environment of every session that culm spawns. A hook process inherits the environment, verified on 2026-09-02.
69. Exit the hook when `CULM_SOCKET` is absent, so that a Claude Code session outside culm costs one process start.
70. Read the session id from the hook payload. The payload field that carries the session id is unverified.
71. Deliver the marker over a Unix socket.
72. Support three states with a fixed priority: needs permission, then needs answer, then done.
73. Give each state an emoji and a color.
74. Clear a marker only when the user resolves the issue. Do not clear a marker when the user focuses the session.
75. Clear a permission marker on the first keystroke culm sends to that session, or on the next hook for that session, whichever comes first.
76. Retire a done marker only on `UserPromptSubmit` or `SessionEnd`. A tool or a subagent that reports in after `Stop` leaves the marker standing, because the hooks of one event run in parallel and reach the socket out of order.
77. Read `notification_type` before a notification moves a marker. Raise the permission marker for `permission_prompt`. Raise the answer marker for `elicitation_dialog`, `elicitation_url_dialog`, and `agent_needs_input`. Move nothing for any other kind.
78. Move nothing for `idle_prompt`. A minute of silence is not a question, and `Stop` has already reported the session idle.

No hook reports that the user answered a permission prompt. `permissionDecision` is a value a hook returns, not an event Claude Code emits, so `PermissionRequest` would otherwise hold the marker until `PostToolUse` lands after the approved tool finishes. Checked on 2026-09-06 against the changelog of the CLI version recorded in [FINDINGS.md](FINDINGS.md), and reproduced by hand: the marker stayed up for the whole time the model worked. The keystroke that answers the prompt is the only signal culm receives, and reading it uses no terminal output, so requirement 65 still holds.
79. Clear a permission marker when the next hook for that session arrives. A declined permission fires no terminating hook, verified on 2026-09-02, so no other signal reports the decline.

The culm process owns the socket. A session runs only while culm runs, so a hook fires only while the socket exists. An unexpected exit of culm loses a marker in flight, and ends the session that produced the marker.

### Interface

The model for the interface is a micro-frontend host. Each session behaves as a terminal that runs on its own. culm arranges those terminals and adds management on top.

80. Render each session as a terminal panel inside the interface.
81. Keep the focused panel interactive. Keyboard input reaches the Claude Code process in the focused panel.
82. Render the whole terminal for the focused panel, not a summary and not a snapshot.
83. Show one session at a time, in the manner of a browser with vertical tabs. A session that is not visible keeps running.
84. Show the attention state of a session on the session list, without stealing focus.
85. Forward a paste to the focused session as one bracketed block.
86. Select text in the focused panel by dragging the mouse over it. Mark the selection on screen. culm holds the mouse for the sidebar and for the wheel, so the host terminal never sees the drag.
87. Copy the selection when the mouse button comes up. Write it to the host terminal through OSC 52, which reaches the system clipboard and the primary selection at once.
88. Paste the last copy with a middle click over the panel, as one bracketed block. `Ctrl+Shift+V` stays with the host terminal and arrives as a bracketed paste.
89. Clear the selection on the next keystroke and on a focus change. A press and a release on one cell is a click, and it selects nothing.
90. Carry the selection when the panel scrolls, so that it keeps marking the text it was put on. A selection carried off the panel marks nothing on screen and still copies its own text, read back out of the scrollback.
91. Measure how far a child moved its own text when culm forwarded the wheel to it, and carry the selection by that distance. Such a child repaints its own panel and reports nothing, so culm hashes the rows before the notch and lines them up against the rows that follow the repaint. Drop the selection when the rows do not line up, because that was a fresh screen and not a scroll. Give the wait an end, so that a child which ignores the wheel never holds a selection.
92. Extend the selection to the clicked cell when `Alt` is held, keeping the anchor where the drag left it. `Alt` rather than `Shift`, because a terminal keeps shift and the mouse for its own selection even while an application holds the mouse: kitty maps `shift+left press` to its own selection for a grabbed application, so that click never reaches culm. Accept `Shift` as well, for a terminal that forwards it.
93. Show an empty state. With no project registered, show the command that registers a project. With no session in the open project, show the binding that creates a session.
94. Run on a terminal that reports no keyboard enhancement. Show the active keyboard mode in the bottom bar at all times.
95. Split the bottom bar in two. Hold the pointer to the shortcut table against the left edge, unchanged at all times, and hold the message of whatever last happened against the right edge.
96. Keep the room the hook warning needs when the bar runs out of width. Shorten the transient message first, then the warning itself, because the warning names a command the user has to type.
97. Show every reserved binding, and what each one does, in a table on `Alt+Shift+H`. Close it with the same binding, with `Esc`, or with `Enter`. Send no key to a child while it is open.
98. Show a search box above the paused list. Filter the paused list by name on every keystroke, ignoring case.
99. Send no key to a child while the search box holds the focus. `Enter` focuses the first match and returns the keys to the panel. `Esc` clears the filter and returns the keys to the panel.
100. Keep the filter after `Enter`, so that the list still shows what was searched for.

### Scrollback

101. Forward a wheel notch to a child that asked for mouse reporting, and let that child scroll its own history. Claude Code asks for it, and holds the conversation itself.
102. Hold the output of a child that asked for no mouse reporting in a scrollback buffer, and let the user look back through it. A shell is such a child.
103. Scroll with the mouse wheel over the panel. Leave the wheel over the sidebar alone.
104. Scroll half a panel with `Shift+PageUp` and `Shift+PageDown`. Leave plain `PageUp` and `PageDown` to the child.
105. Return the view to the live output on the first keystroke the user sends, as a terminal does.
106. Hold the view still while output arrives, so that a busy session never drags the view.
107. Show the offset in the panel title while the view sits above the live output, so that a held panel never reads as a stalled session.
108. Scroll nothing while a form is open.

A child that paints its own viewport never lets a line scroll off the panel, so no
scrollback accumulates for it and culm has nothing of its own to show. `vt100` also
keeps no scrollback while a scroll region is active, in `grid.rs`. Requirement 101 is
what makes such a session scrollable, and requirements 102 to 107 cover the rest.

### Keys

Every reserved key stops belonging to the Claude Code process. Keep the reserved set small.

109. Use `Alt` as the only leader. Reserve no function key.
110. Reserve `Ctrl+q` for quit.
111. Send `ESC` then `CR` for `Shift+Enter`, which is the sequence Claude Code binds to a newline that does not submit. Send a bare `CR` for `Enter`.
112. Focus a session with `Alt` plus a digit from 1 to 9. Focus the shell with `Alt+0`.
113. Hold at most nine active sessions in a project. Refuse a tenth, and report the limit. The shell is not a session and does not count, so a full project shows ten panels.
114. Create a session with `Alt+Shift+N`. A new session starts active.
115. Pause the focused active session, and resume the focused paused session, with `Alt+Shift+P`.
116. Toggle nerd mode with `Alt+Shift+D`.
117. Delete the focused session with `Alt+Shift+X`. Offer the binding only for a paused session, which is how requirement 43 reaches the user.
118. Rename the focused session with `Alt+Shift+R`.
119. Focus the search box with `Alt+Shift+F`. A click on the search row focuses it as well.
120. Open the shortcut table with `Alt+Shift+H`.
121. Address the active half with a digit. Reach a paused session with a mouse click, or through the search box.

### Configuration

122. Read one configuration file in TOML form from `~/.config/culm/config.toml`.
123. Hold every key binding in the configuration file. Ship the defaults in the binary.
124. Override one binding without restating the rest.
125. Report an unknown action name and an unparsable binding at load. Name the file and the line. Do not ignore the entry.
126. Read the nerd mode default from the same file.

### Statistics

127. Show the resident memory of each session on its sidebar row. Read `VmRSS` from `/proc/<pid>/status` and sum the process tree of the session.
128. Sample memory once per second. Render the last sample.
129. Show the frame rate in the top right corner under nerd mode. Keep nerd mode off by default.
130. Reach `/proc` through a trait, so that a test supplies a fake.

One Claude Code process measured about 436 MB of resident memory on 2026-09-02. Memory, and not render cost, is the limit on the session count.

### Persistence

131. On closing a project, pause every session and record which sessions were active.
132. On opening a project, restore both halves of the session list to the recorded state.
133. Survive a reboot through the saved state, because a reboot ends every terminal process.

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
| 14 | Copying out of a panel | culm owns the selection. A drag marks it, a release copies it through OSC 52, and a middle click pastes it. The earlier plan, a binding that turns mouse capture off, is dropped. | 2026-09-06 |
| 15 | Naming an imported session | The transcript title when it holds one. Otherwise a headless `claude -p --model haiku` child reads the first prompt and answers with a name. Otherwise the first eight characters of the id. | 2026-09-06 |
| 16 | Confirming a session delete | Two buttons, with the cursor on no. `y` and `n` move the cursor, `Enter` answers, and a click answers. The typed name is dropped, because retyping a name is friction rather than safety when the default answer is already no. | 2026-09-07 |

## Deferred

Crash survival. culm holds the master side of each pseudoterminal. When culm exits, the master closes, and the kernel hangs up each child. An unexpected exit of the interface therefore ends every session in the project.

The loss is bounded. The Claude Code CLI writes the transcript during the run, so `--resume` recovers every completed turn. An unexpected exit loses the turn in flight, and costs one process start plus one transcript reload for each active session.

Two designs remove the loss. A culm daemon holds every master and the interface attaches over a socket. One `abduco` process per session holds one master each, and culm attaches as a client. Both changes land behind `PtySpawner` in `src/pty.rs`, which is the only place culm starts a process.

Build neither until the loss becomes real. Order of work is to make it work first.

## Open questions

None stand on 2026-09-06.
