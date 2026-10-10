# Architecture

This document explains how culm is built, and the assumptions the code makes without saying so. [CLAUDE.md](CLAUDE.md) holds the goals and the rules for working in the repository. This document holds the mechanism.

## The shape of the program

culm is one process. It owns every child it starts, and nothing outside it supervises them. When culm exits, the kernel hangs up every pseudoterminal, and every session ends.

The process holds one hierarchy:

| Level | Type | Holds |
| --- | --- | --- |
| Window | `Browser` in `src/browser.rs` | Up to eight tabs, the window's own forms, the settings, nerd mode, and the last copy. |
| Tab | `Tab` in `src/browser.rs` | One `App`, and the channel that carries that project's hooks. |
| Project | `App` in `src/app.rs` | The sessions, the shell, the two panes, the sidebar width, the project's forms, and its bottom bar message. |
| Session | `Entry` in `src/app.rs` | The saved record, the running `Session` if any, the attention marker, and the memory sample. |
| Child | `Session` in `src/session.rs` | One pseudoterminal, one `vt100` parser, and one drain thread. |

`Browser` decides which tab gets an event. `App` decides what the event does inside one project. Neither one reads input or writes output. `src/main.rs` reads the terminal events, calls the library, draws, and writes the clipboard escape. Nothing else lives there.

## Modules

| Path | Holds |
| --- | --- |
| `src/main.rs` | Terminal setup, the event loop, and teardown. |
| `src/lib.rs` | The module list. |
| `src/browser.rs` | The window: tabs, tab keys, the project picker, closing a tab, and the quit with its workspace prompt. |
| `src/app.rs` | One project: sessions, focus, panes, forms, archive, recap, and close. |
| `src/session.rs` | One session: its child, its screen, its scrollback, and the thread that drains it. |
| `src/pty.rs` | The process boundary: `SessionSpec`, `Pty`, `Child`, and `PtySpawner`. |
| `src/keys.rs` | Host actions and key encoding. Pure functions. |
| `src/ui.rs` | Rendering, layout, and the hit boxes that map a click back to a tab, a row, or a cell. |
| `src/hooks.rs` | Attention markers: the hook settings merge, the marker state machine, and the socket. |
| `src/project.rs` | Projects, session records, the registry, and slugs. Pure values. |
| `src/workspace.rs` | Workspaces. Pure values. |
| `src/store.rs` | Every file read and write, behind `Store`. |
| `src/cli.rs` | The command line. Every project, repository, and workspace operation. |
| `src/recap.rs` | The headless recap child, behind `Recapper`. |
| `src/namer.rs` | The headless child that names an imported session, behind `Namer`. |
| `src/transcript.rs` | Reads the head of a Claude Code transcript for an import. |
| `src/config.rs` | The settings that apply to every project. |
| `src/stats.rs` | Memory sampling from `/proc`, behind `MemoryProbe`. |
| `src/clock.rs` | The time, behind `Clock`. |
| `src/git.rs` | Worktree commands, behind `Git`. |
| `src/clipboard.rs` | The OSC 52 escape. |
| `src/testing.rs` | The fakes. Compiled into the library, so the tests under `tests/` reach them. |

## The event loop

`src/main.rs` runs one loop. Each pass does these steps in order:

1. Wait up to 4 ms for a terminal event. A key, a paste, or a mouse event goes to `Browser`. After a mouse event, the loop writes a finished copy to the terminal as OSC 52.
2. `Browser::poll`: drain the hook channel of every tab, carry every recap forward, measure a forwarded scroll on the focused tab, and carry every close forward.
3. Once a second, `Browser::on_tick`: reap a child that ended, sample memory, restart an exited shell, and save a project with unsaved changes.
4. Draw at most once per 8 ms. A session that prints at full speed never drives the frame rate.
5. Resize every tab's sessions to that tab's panel, the tabs not on screen included.

Input comes first, so a keystroke never waits behind a frame. Every wait in culm runs a pass at a time. A quit, a recap, and a scroll measurement each keep the screen drawing while they wait.

## Sessions and their children

Each session is a real `claude` process on its own pseudoterminal, opened with `portable-pty`. The environment of every child carries `TERM=xterm-256color` and `COLORTERM=truecolor`.

A drain thread per session reads the pseudoterminal and feeds a `vt100` parser with 2000 lines of scrollback. The thread runs for every session, on screen or not. A pseudoterminal that nobody reads fills its buffer, and the child then blocks on its next write. The symptom looks like a hung session in the background. A frame copies only the focused session's screen into the ratatui buffer, so a tab or session switch costs nothing.

The pseudoterminal slave is dropped right after the spawn. A slave that stays open in culm keeps the master from reaching end of file, so culm never sees the child exit.

### Identity and resume

- culm generates a UUID v4 for a new session and passes `--session-id <uuid>`.
- Every later start passes `--resume <uuid>`, which reloads the whole conversation.
- The `started` flag in the record chooses between the two.
- culm keeps the session id, the name, the slug, the working directory, and the repositories. It never keeps the conversation.

### States

A session record holds one of three states:

| State | Process | Shown in |
| --- | --- | --- |
| `active` | Running. | The active list of the sidebar. |
| `paused` | None. | The paused list of the sidebar, and the paused search. |
| `archived` | None. | The archive list only. `archived_from` holds the list a restore returns it to. |

- A pause sends SIGTERM and waits up to three seconds. An in-flight tool call is lost.
- A project holds at most eight active sessions. The shell does not count.
- A project saved with more opens the first eight and leaves the rest paused.

### Order

- The active list and the paused list keep the order the user gives them with `J` and `K`.
- A session that enters the paused list goes to its top, through `App::lift`, so an untouched list reads newest first.
- The archive is sorted by `last_active`, newest first.
- `reorder` only partitions the entries into the three lists. It never sorts the first two.

### The shell

Each project starts `$SHELL` at its root, at the top of its sidebar. The shell uses the same pseudoterminal code as a session, without a session id, a marker, a worktree, or `CULM_SOCKET`. culm never saves it. A shell that exits restarts on the next tick, except while its project closes.

## Keys

`keys::host_action` decides which keys culm keeps. Every other key goes to `keys::encode` and then to the focused child.

- `Alt` is the only leader. culm keeps `Alt` plus a digit from 1 to 8, `Alt+0`, a set of `Alt+Shift` letters, `Ctrl+q`, and `Shift+PageUp` and `Shift+PageDown`.
- An `Alt+Shift` letter arrives as an uppercase letter on the legacy path, and as a lowercase letter with the shift modifier on the kitty path. `host_action` accepts both forms.
- culm asks the terminal for the kitty keyboard protocol with `DISAMBIGUATE_ESCAPE_CODES`. A terminal that refuses runs the legacy path. The bottom bar names the mode.
- `encode` follows the cursor key mode of the child. When the child sets DECCKM, as zsh does through terminfo `smkx`, arrows, `Home`, and `End` go out as `ESC O x`. Otherwise they go out as `ESC [ x`.
- `Shift+Enter` goes out as `ESC CR`, the sequence Claude Code binds to a new line in `/terminal-setup`.
- `Ctrl+Backspace` goes out as `ESC DEL`, which zsh, bash, and Claude Code read as a word delete.

### Panes

Each project has two panes: the sidebar and the panel. One pane holds the keys.

- While the sidebar holds the keys, no key and no paste reaches a child.
- The keys follow the mouse. A move, a drag, or a press over a pane gives that pane the keys.
- A drag of the separator never moves the keys.
- `HitBox::in_sidebar` counts only the rows the panel spans, so the tab bar row is never part of the sidebar.

## The mouse

culm turns on full mouse reporting, so the terminal reports every move.

- **Tab bar:** a press on a tab focuses the tab and picks it up. A drag then carries the tab past every tab it crosses, by column only, until the release. Every other event goes to the focused project, a release over the tab bar included, so a drag inside a project never stays half done.
- **Selection:** a drag over the panel selects cells, and the release copies them through OSC 52. culm draws the selection over the finished buffer, so the child never learns of it.
- **Wheel:** a child that asked for mouse reporting gets the wheel, as Claude Code does. For other children, culm scrolls its own `vt100` scrollback.
- **Selection during a scroll:** a child that scrolls itself reports no distance. culm hashes the rows before the notch and lines them up against the rows after the repaint, then moves the selection by that offset. Rows that do not line up mean a fresh screen, and culm drops the selection.

## Attention markers

`culm hooks install` writes nine hook events into `~/.claude/settings.json`. Each entry runs `<path of the culm binary> hook`.

1. Claude Code runs the hook with the payload on stdin.
2. `culm hook` reads `CULM_SOCKET` from its environment. With no variable, it exits at once, so a `claude` started outside culm costs one process start.
3. `culm hook` writes the payload to the socket, with a 200 ms write timeout.
4. The tab of the project reads the payload from its channel. `Attention::apply` moves the marker of the session with that id.

- **Socket:** each open project listens on its own socket, `$XDG_RUNTIME_DIR/culm-<slug>.sock` (or `/tmp/culm-<uid>-<slug>.sock`), through `HookListener`.
- **Priority:** the marker order is permission, then answer, then done. `Attention` derives `Ord` in that order, so a tab shows the `max` of its sessions.
- **Answered permission:** no hook reports that the user answered a permission prompt. The keystroke that reaches the session clears the permission marker.
- **No output parsing:** culm reads no terminal output to decide a state.

## State on disk

| File | Holds |
| --- | --- |
| `$XDG_STATE_HOME/culm/registry.json` | Every project: slug and root. |
| `$XDG_STATE_HOME/culm/projects/<slug>.json` | One project: root, repositories, session records, and the `view` block with the sidebar width. |
| `$XDG_STATE_HOME/culm/workspaces.json` | Every workspace: name and project slugs, in tab order. |
| `$XDG_CONFIG_HOME/culm/config.toml` | The settings for every project. A missing file reads as the defaults. |

- `XDG_STATE_HOME` defaults to `~/.local/state`, and `XDG_CONFIG_HOME` defaults to `~/.config`.
- Every write goes to a temporary file and then renames it into place, so an interrupted write never truncates a file.
- A keystroke marks the project dirty, and the next tick writes it, so typing never costs a file write.
- A field added to a record carries `#[serde(default)]`, so an older file still reads.
- A new enum variant is the exception. culm 0.1.1 refuses a project file that holds an `archived` session.

## Worktrees

A session that edits a repository gets `git worktree add -b <session-slug>` under `<project root>/.worktrees/<repo>-<session-slug>`. Each chosen repository gets one worktree, and every worktree of the session shares one branch name. A rename changes the displayed name only, because a running session holds its paths open. culm creates and removes worktrees. It never commits, never pushes, and never deletes a branch.

## Closing

- **Quit:** `Ctrl+q` asks every child of every tab to leave, and gives them one grace period of three seconds together. A second `Ctrl+q` stops the wait, and culm kills what is left. A forced quit is recorded in the project, and the next open reports it for five seconds.
- **Closing one tab:** `Alt+Shift+W` moves the tab into `closing_tabs`. Its children leave in the background while the other tabs keep running.
- **Save prompt:** a quit asks about the workspace when more than one project is open outside a workspace, or when the tabs differ from their workspace in content or in order. `needs_save_prompt` holds the rule. The prompt asks first, and asks no child to leave until the user answers.
- **Recaps:** a close kills a recap that still runs, so no headless child outlives culm.

## Headless children

Two features run `claude` without a terminal:

| Feature | Command | Why the flags |
| --- | --- | --- |
| Recap on pause | `claude -p --no-session-persistence --resume <id> /recap` | Without `--no-session-persistence`, the recap appends to the real transcript and moves its time. |
| Naming an import | `claude -p --bare --model haiku --no-session-persistence --tools "" --output-format text` | `--bare` skips the hooks, so naming posts no marker. `--tools ""` leaves the child no way to touch the machine. |

A recap runs only after the child is gone, because Claude Code refuses to resume a session that it still lists as running. A recap that takes more than 45 seconds is abandoned.

## Assumptions the code makes without saying so

These hold today. A change that breaks one of them breaks culm in a way no test catches.

### About Claude Code

- Claude Code writes the transcript of a session to `~/.claude/projects/<dir>/<uuid>.jsonl`. `<dir>` is the session's working directory, with every `/` and every `.` replaced by `-`. culm derives the path with `transcript_dir_name` and never asks Claude Code.
- `--resume <uuid>` works only from the working directory the transcript was written in. A session's `cwd` therefore never changes after creation.
- A session with no transcript has never received a prompt. A restore from the archive treats a missing transcript as a session that is gone.
- The hook payload carries `session_id`, `hook_event_name`, `tool_name`, and `notification_type`. culm reads no other field.
- Claude Code asks for mouse reporting and paints its own viewport. Nothing scrolls off its screen, so `vt100` holds no scrollback for it.
- Claude Code on Linux reads byte `0x08` as `Ctrl+Backspace` and `ESC DEL` as `Meta+Backspace`. Both delete a word.
- Claude Code reads both `ESC [ H` and `ESC O H` as `Home`, and does not set DECCKM.

### About the terminal

- The host terminal delivers `Alt` as a modifier or as an `ESC` prefix. A terminal that keeps `Alt` for itself takes away every culm shortcut.
- The terminal answers OSC 52, so a copy reaches the system clipboard. culm never reads the clipboard back. A middle click pastes culm's own last copy.
- The sixteen theme colors can repeat. gruvbox gives bright white and bright black the same value. A text color and its fill must never both come from the sixteen. The sidebar highlight uses color 237 of the 256-color ramp and the terminal's own foreground for that reason.

### About the machine

- One user runs culm on one machine. culm takes no file lock on the state files. Two windows on different projects are safe, because each project has its own state file.
- A socket that answers a connection belongs to a live culm. A socket file that refuses is left over from a crash, and culm replaces it.
- `culm hooks install` records the path of the binary. Moving the binary breaks every hook until `culm hooks install` runs again.
- `/proc` exists, so memory sampling works. Memory sampling is the only Linux-only code path besides the release packages.

### About identifiers

- A slug is the identifier of a project and of a workspace. It names the state file, the socket, and the entries inside workspaces. A project rename moves the state file and rewrites every workspace that holds the project.
- A session slug names the branch and the worktree directories, and never changes.
- A digit addresses a tab, and only a tab. Eight tabs and eight active sessions keep every count within one digit.

## Testing

Every side effect enters through a trait, and each trait has a fake in `src/testing.rs`:

| Trait | Real | Fake |
| --- | --- | --- |
| `PtySpawner` | `SystemPtySpawner` | `FakeSpawner` replays fixed output and records every write. |
| `Git` | `SystemGit` | `FakeGit` records worktree calls. |
| `Store` | `FsStore` | `MemoryStore` keeps every file in memory and counts the writes. |
| `Clock` | `SystemClock` | `FakeClock` moves only when a test sets it. |
| `Recapper` | `ClaudeRecapper` | `FakeRecapper` answers after a set number of passes and counts kills. |
| `HookListener` | `SocketHookListener` | `FakeHookListener` hands the test a sender per project. |
| `MemoryProbe` | `ProcMemoryProbe` | `FakeMemoryProbe` returns set values. |
| `Namer` | `ClaudeNamer` | `FakeNamer` answers by prompt text. |

`tests/session_lifecycle.rs` drives one project through `App`. `tests/browser.rs` drives the window through `Browser`, including the command line. `tests/import.rs` covers the import of existing transcripts. The tests start no process and write only inside a temporary directory.

No test covers the real terminal: rendering in kitty, the kitty keyboard protocol, OSC 52, and the real hook socket. The maintainer checks those by hand before a commit.
