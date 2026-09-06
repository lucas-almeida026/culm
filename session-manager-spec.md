# Session manager — spec

Goals for a tool that manages many Claude Code sessions across several repositories. Written 2026-09-06. The comparison that produced these decisions is in [tmux-claude-setup.md](tmux-claude-setup.md), which also records the current shell-based setup that this tool would replace.

## Purpose

Run many Claude Code sessions at the same time, one per feature. Group them by project. Return to any session later with its full conversation intact. Show which session needs attention and why.

## Concepts

| Term | Definition |
| --- | --- |
| Project | A managed directory. Any directory qualifies. A project needs no git repository at its root. |
| Repository | A git repository that belongs to a project. A repository lives inside the project directory or outside it. |
| Session | One Claude Code process, with one transcript, one name, and one attention state. |
| Worktree | A git worktree plus a branch, created for one session in one repository, to isolate parallel edits. |

## Decisions

| Dimension | Decision |
| --- | --- |
| Runs in a non-git directory | Yes. The tool manages projects itself and accepts any directory as a project root. |
| Unit of organization | The project. One project per managed directory. |
| Cross-repository work in one session | Yes. A session reaches every repository in its project. The tool adds and removes repositories, including repositories outside the project root. |
| Parallel edits to the same repository | Yes, through git worktrees and a dedicated branch per session. |
| Resume an existing transcript | Yes. The tool runs `claude -r` to reload the whole transcript. The tool also deletes a Claude session, whether or not the session belongs to a managed project. |
| Migrated history | Out of scope. The 2026-09-02 path migration was a one-time task. |
| Transcript storage | Native. The tool never changes where the Claude Code CLI writes transcripts. |
| Attention signal source | Harness hooks. |
| Attention granularity | Three states: needs permission, needs answer, done. Priority-ordered, emoji-coded, and color-coded. |
| Marker clears when | The user resolves the issue through interaction. |
| Non-code sessions | Native, with no difference in handling. |
| Session naming | Manual, with no default pattern. A rename applies to an existing session. |
| Git diff, commit, and push | Out of scope. |
| Auto-accept prompts | Out of scope, by choice. |
| Model and effort per session | Yes. Each session is a real terminal process that runs the Claude Code CLI, and the tool relays Claude commands to that terminal. |
| Reboot survival | Automatic. Closing a project pauses every session and saves which sessions were running. Opening the project restores the interface to that state. |
| Session rendering | Embedded. The tool renders the focused session as a full interactive terminal panel inside its own interface. One session is visible at a time. Every session runs whether or not it is visible. |
| Footprint | One Go binary plus configuration files. |
| Dependencies | tmux, jq, gh. |

## Requirements

### Projects

1. Register a project from any directory. Do not require a git repository at the project root.
2. Add a repository to a project. Accept a path inside the project root and a path outside it.
3. Remove a repository from a project without deleting the repository.
4. List projects, and open one project at a time in the interface.

### Sessions

5. Create a session inside a project. Take the name from the user. Apply no default pattern.
6. Rename a session after creation.
7. Run each session as a real terminal process that runs the Claude Code CLI, so that Claude commands, model selection, and effort selection reach the process without translation.
8. Attach to a session and detach from it without ending the process.
9. Resume a session with `claude -r`, which reloads the whole transcript.
10. Delete a Claude session, including a session that no managed project owns. Deletion removes the transcript, so require a confirmation.

### Parallel edits

11. Create a git worktree and a dedicated branch for a session that edits a repository, so that two sessions editing one repository do not collide.
12. When a project holds no git repository, run without worktrees. Suggest that the user creates a repository. Allow the user to continue with one active session at a time, as a plain wrapper.

### Attention markers

13. Set the marker from Claude Code hooks, not from terminal output. Hooks inherit `$TMUX_PANE` and `$CLAUDE_CODE_SESSION_ID`, which is how a hook finds the session it belongs to. Verified on 2026-09-02 with Claude Code 2.1.258.
14. Support three states with a fixed priority: needs permission, then needs answer, then done.
15. Give each state an emoji and a color.
16. Clear a marker only when the user resolves the issue. Do not clear a marker when the user focuses the session.

### Interface

The model for the interface is a micro-frontend host. Each session behaves as a terminal that runs on its own. The tool arranges those terminals and adds management on top.

17. Render each session as a terminal panel inside the interface.
18. Keep every panel interactive. Keyboard input reaches the Claude Code process in the panel that holds focus.
19. Render the whole terminal, not a summary or a snapshot, for the panel that holds focus.
20. Show one session at a time. The interface renders the focused session only, in the manner of a browser with vertical tabs. A session that is not visible keeps running.
21. Show the attention state of a session on its panel, and on the session list, without stealing focus.

### Persistence

22. On closing a project, pause every session and record which sessions were running.
23. On opening a project, restore the interface to the recorded state.
24. Survive a reboot through the saved state, because a reboot ends every terminal process.

## Out of scope

- Git diff, commit, push, and any other version control action beyond worktree and branch management.
- Automatic acceptance of permission prompts.
- Any change to where the Claude Code CLI writes transcripts.
- Migration of transcripts between project directories.

## Open questions

1. **Eager or lazy resume.** Requirement 9 reloads the whole transcript. Some transcripts reach 18 MB, so restoring eight sessions at once is slow and costly. Decide whether opening a project resumes every session at once, or whether a session resumes on first attach.
2. **Worktrees for a cross-repository session.** A session reaches several repositories, and requirement 11 gives a worktree per repository. Decide whether a session creates a worktree in every repository of the project, or only in repositories the user names, and whether the branch name is shared across those worktrees.
3. **Meaning of pause.** The Claude Code CLI has no pause. A pause ends the process and records the session id for a later `claude -r`. Confirm that in-flight tool calls may be lost, and decide whether pause waits for an idle session.
4. **Hook installation.** Markers need hook entries in `~/.claude/settings.json`, which apply to every Claude Code session on the machine. Decide how the tool installs those entries, how it leaves unrelated sessions untouched, and how it removes the entries on uninstall.
5. **A declined permission fires no terminating hook.** Only `PreToolUse` and `PermissionRequest` fire, so a permission marker cannot clear on a decline. Verified on 2026-09-02. Decide whether the marker persists until the next prompt, or whether the tool polls for the resolution.
6. **The jq dependency.** A Go binary parses JSON without jq. Confirm whether jq stays for the hook script, or whether the binary receives hook payloads directly.
7. **Deleting a session that is running.** Decide whether deletion refuses while the process is alive, or ends the process first.
8. **Interface surface.** Decide between a terminal application that draws panels inside one terminal window, and a desktop application that draws panels in a window of its own. The choice fixes the language, the terminal emulator component, and the amount of terminal handling to write.
9. **Process supervision.** Decide whether the tool owns the pseudoterminal of each session, or whether tmux owns each process and the tool attaches to it. A tmux layer keeps sessions alive when the interface exits or crashes, and it keeps a session reachable from a plain terminal. Direct ownership removes a dependency and one layer of resizing.
10. **Panel size.** Two clients attached to one tmux session share one view and shrink to the smaller size. A panel per session therefore needs one tmux session per Claude session, or a grouped session. Verified with tmux 3.4 on 2026-09-02.
11. **Inactive panels.** Rendering many live terminals at once costs processing. Decide whether a panel that does not hold focus renders live output, or a periodic snapshot.
12. **Keyboard fidelity.** The Claude Code interface uses `shift+tab`, `escape`, `ctrl+r`, and extended keys. Confirm that the embedded terminal forwards those keys unchanged, and that the tool's own shortcuts do not consume them.
