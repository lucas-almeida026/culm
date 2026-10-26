# Architecture

This document explains how culm is built, and the assumptions the code makes without saying so. [CLAUDE.md](CLAUDE.md) holds the goals and the rules for working in the repository. This document holds the mechanism.

## The shape of the program

culm is one process. It owns every child it starts, and nothing outside it supervises them. When culm exits, the kernel hangs up every pseudoterminal, and every session ends.

The process holds one hierarchy:

| Level | Type | Holds |
| --- | --- | --- |
| Window | `Browser` in `src/browser.rs` | Up to eight tabs, the window's own forms, the settings, nerd mode, and the last copy. |
| Tab | `Tab` in `src/browser.rs` | One `App`, and the channel that carries that project's hooks. |
| Project | `App` in `src/app.rs` | The sessions, the shell, the panes, the changes panel, the sidebar width, the project's forms, and its bottom bar message. |
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
| `src/hooks.rs` | Attention markers: the hook settings merge, the marker state machine, and the socket, which also carries commands. |
| `src/project.rs` | Projects, session records, the registry, and slugs. Pure values. |
| `src/workspace.rs` | Workspaces. Pure values. |
| `src/store.rs` | Every file read and write, behind `Store`. |
| `src/cli.rs` | The command line. Every project, repository, workspace, and session registration operation, and the adoption of existing worktrees. |
| `src/command.rs` | What a session registers: its ticket, its port number, and its worktrees. `apply` holds every rule, and both the window and the command line call it. Pure. |
| `src/recap.rs` | The headless recap child, behind `Recapper`. |
| `src/namer.rs` | The headless child that names an imported session, behind `Namer`. |
| `src/copytext.rs` | Rebuilds a copy of a Claude Code panel: the copied words found in the transcript come back as their markdown, and the rest of the screen lines are joined where they only met the edge. Pure functions. |
| `src/transcript.rs` | Reads a Claude Code transcript: its head for an import, and the files a session wrote for the changes panel. |
| `src/config.rs` | The settings that apply to every project. |
| `src/stats.rs` | Memory sampling from `/proc`, behind `MemoryProbe`. |
| `src/clock.rs` | The time, behind `Clock`. |
| `src/mic.rs` | The microphone a session records from, picked from `pactl` output, behind `Microphones`. |
| `src/git.rs` | The read-only git reads of the changes panel and of a worktree registration, behind `Git`. |
| `src/diff.rs` | The changes panel's data: git's output parsed, two versions of a file laid side by side, and syntax colors. Pure functions, except the color grammars it loads. |
| `src/gitview.rs` | The state of the changes panel: repository tabs, lists, picks, the diff on screen, keys, and clicks. |
| `src/gitui.rs` | Draws the changes panel. |
| `src/clipboard.rs` | The OSC 52 escape. |
| `src/testing.rs` | The fakes. Compiled into the library, so the tests under `tests/` reach them. |

## The event loop

`src/main.rs` runs one loop. Each pass does these steps in order:

1. Wait up to 4 ms for a terminal event. A key, a paste, or a mouse event goes to `Browser`. After a mouse event, the loop writes a finished copy to the terminal as OSC 52.
2. `Browser::poll`: drain the hook channel of every tab, carry every recap forward, measure a forwarded scroll on the focused tab, let the changes panel of the focused tab read git when something asked for it, and carry every close forward.
3. Once a second, `Browser::on_tick`: reap a child that ended, sample memory, restart an exited shell, and save a project with unsaved changes.
4. Draw at most once per 8 ms. A session that prints at full speed never drives the frame rate.
5. Resize every tab's sessions to that tab's panel, the tabs not on screen included.

Input comes first, so a keystroke never waits behind a frame. Every wait in culm runs a pass at a time. A quit, a recap, and a scroll measurement each keep the screen drawing while they wait.

## Sessions and their children

Each session is a real `claude` process on its own pseudoterminal, opened with `portable-pty`. The environment of every child carries `TERM=xterm-256color` and `COLORTERM=truecolor`.

Each session start also asks the sound server for its sources through `Microphones`, and sets `PULSE_SOURCE` to the wanted one. Claude Code's voice dictation records from that source. `src/mic.rs` fixes the choice for the maintainer's machine: a source whose name or description holds `NoiseTorch`, then one that holds `Family 17h/19h`. A monitor source never matches. With no match, or with no `pactl`, the session starts on the system default, and the status bar shows a warning under every other message until a later start finds a source. A running session keeps the source it started with. Evidence: `a_session_records_from_the_wanted_microphone`, `a_session_with_no_wanted_microphone_starts_on_the_default_and_says_so`, and `a_resume_picks_a_microphone_that_appeared_after_the_first_start` in `tests/session_lifecycle.rs`, and the tests in `src/mic.rs`. The shell gets no `PULSE_SOURCE`.

A drain thread per session reads the pseudoterminal and feeds a `vt100` parser with 2000 lines of scrollback. The thread runs for every session, on screen or not. A pseudoterminal that nobody reads fills its buffer, and the child then blocks on its next write. The symptom looks like a hung session in the background. A frame copies only the focused session's screen into the ratatui buffer, so a tab or session switch costs nothing.

The pseudoterminal slave is dropped right after the spawn. A slave that stays open in culm keeps the master from reaching end of file, so culm never sees the child exit.

### Identity and resume

- culm generates a UUID v4 for a new session and passes `--session-id <uuid>`.
- Every later start passes `--resume <uuid>`, which reloads the whole conversation.
- The `started` flag in the record chooses between the two.
- culm keeps the session id, the name, the slug, the working directory, and what the session registered: its worktrees, its ticket, and its port number. It never keeps the conversation.
- Every session starts at the project root. Its environment carries `CULM_SESSION` (its id) and `CULM_PROJECT` (the project slug), so `culm session` run inside it needs no flag.
- A session that registered something gets it at each start through `--append-system-prompt`: the ticket, the port number, and each worktree with its branch. A session that registered nothing gets no system prompt from culm.

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
- Plain `PageUp` and `PageDown` scroll culm's scrollback of the panel, as `Shift+PageUp` and `Shift+PageDown` do. A child on the alternate screen, such as vim, less, or Claude Code's fullscreen renderer, gets the key instead, because such a child keeps no scrollback in culm and pages its own text. Evidence: `plain_page_up_scrolls_the_history_of_a_session_on_the_main_screen` and `a_full_screen_program_pages_its_own_text` in `tests/session_lifecycle.rs`.
- An `Alt+Shift` letter arrives as an uppercase letter on the legacy path, and as a lowercase letter with the shift modifier on the kitty path. `host_action` accepts both forms.
- culm asks the terminal for the kitty keyboard protocol with `DISAMBIGUATE_ESCAPE_CODES`. A terminal that refuses runs the legacy path. The bottom bar names the mode.
- `encode` follows the cursor key mode of the child. When the child sets DECCKM, as zsh does through terminfo `smkx`, arrows, `Home`, and `End` go out as `ESC O x`. Otherwise they go out as `ESC [ x`.
- `Shift+Enter` goes out as `ESC CR`, the sequence Claude Code binds to a new line in `/terminal-setup`.
- `Ctrl+Backspace` goes out as `ESC DEL`, which zsh, bash, and Claude Code read as a word delete.
- `Ctrl+Enter` goes out to a Claude Code session as `CSI 13;5u`. Claude Code 2.1.310 parses that form and binds `ctrl+enter` to `chat:sendNow`, checked on 2026-10-24 in the strings of the binary. The shell gets `CR`, as a plain terminal sends. On the legacy keyboard path the terminal reports `Ctrl+Enter` as `Enter`, so the key arrives as `Enter`.
- With `block-ctrl-z` on, the default, `Ctrl+z` reaches no Claude Code session. Claude Code suspends itself on the key, and no shell above it in culm runs `fg`. The shell gets the key.
- `Alt+Shift+Z` takes back the last paste in a Claude Code session. `src/paste.rs` holds the rules. culm never sees the prompt, so culm sends Claude Code's own undo, `Ctrl+_`. Claude Code merges edits less than about one second apart into one undo step, so culm holds a paste until 1000 ms (`PASTE_GAP_MS`) after the last input that session received. A key during the wait sends the held paste first. The same text pasted twice expands a collapsed paste, so its undo sends `Ctrl+_` twice. `Ctrl+v` counts as a paste, because Claude Code fills it from the clipboard. A paste in the shell never waits.
- While the sidebar holds the keys, only `/` starts the paused search. A letter does nothing, so a letter typed in the wrong pane never lands in the search box.

### Panes

Each project has up to three panes: the sidebar, the panel, and the changes panel while it is open. One pane holds the keys. `Alt+Shift+O` steps through the panes on screen, and skips the panel while the changes panel covers it.

- While the sidebar or the changes panel holds the keys, no key and no paste reaches a child.
- The keys follow the mouse. A move, a drag, or a press over a pane gives that pane the keys.
- A drag of the separator never moves the keys.
- `HitBox::in_sidebar` counts only the rows the panel spans, so the tab bar row is never part of the sidebar.

## The mouse

culm turns on full mouse reporting, so the terminal reports every move.

- **Tab bar:** a press on a tab focuses the tab and picks it up. A drag then carries the tab past every tab it crosses, by column only, until the release. Every other event goes to the focused project, a release over the tab bar included, so a drag inside a project never stays half done.
- **Selection:** a drag over the panel selects cells, and the release copies them through OSC 52. culm draws the selection over the finished buffer, so the child never learns of it.
- **Wheel:** a child that asked for mouse reporting gets the wheel, as Claude Code does. For other children, culm scrolls its own `vt100` scrollback.
- **Markdown copy:** with `copy-markdown` on, a copy of a Claude Code session goes through `copytext::rebuild` before OSC 52. culm reads the session's transcript at the release, indexes every answer and typed prompt by word, and finds the longest runs of copied words, three words at least, inside one answer or prompt. Six screen rows above the selection break a tie between two equal runs. A run comes back as its markdown, with a code span or a bold run the selection cut open closed again. Screen text outside every run keeps its rows, except a row that only met the panel edge: it joins the row above when it opens no block, sits at the indent of the text above within one space, and its first word would not fit in the columns left above, two kept free. The shell copies as drawn. On 2026-10-26 a release build read and indexed a 7.9 MB transcript in 15 ms, and rebuilt one selection in at most 2.1 ms. Evidence: the tests in `src/copytext.rs`, and `a_markdown_copy_hands_over_the_answer_as_its_transcript_holds_it` and `a_markdown_copy_without_a_transcript_still_joins_the_drawn_lines` in `tests/session_lifecycle.rs`.
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

### Commands on the same socket

The window owns the state of an open project and writes it on the tick, so a command line that wrote the file would lose its change on the next tick. A `culm session` command therefore goes through the socket when the project is open.

1. The command line asks `HookListener::is_open`. A closed project is changed in its file with `command::apply`, and the command ends there.
2. For an open project, the command line writes `{"culm": <command>}`, closes its write half, and waits up to 3 s for the reply.
3. The listener tells a command from a hook payload by the `culm` key. A hook payload never carries it.
4. The listener hands the command and a reply channel to the tab, and waits for the reply on a thread of its own, up to 2 s, so hooks keep flowing.
5. `Browser::poll` gives the command to `App::command`, which calls `command::apply`, marks the project dirty, and sends the reply back.

A refused command makes the command line exit with an error, so a script sees it.

## State on disk

| File | Holds |
| --- | --- |
| `$XDG_STATE_HOME/culm/registry.json` | Every project: slug and root. |
| `$XDG_STATE_HOME/culm/projects/<slug>.json` | One project: root, repositories, session records, and the `view` block. The `view` block holds the sidebar width in columns, and the shares of the changes panel and its list in percent once the user dragged them. |
| `$XDG_STATE_HOME/culm/workspaces.json` | Every workspace: name and project slugs, in tab order. |
| `$XDG_CONFIG_HOME/culm/config.toml` | The settings for every project. A missing file reads as the defaults. |

- `XDG_STATE_HOME` defaults to `~/.local/state`, and `XDG_CONFIG_HOME` defaults to `~/.config`.
- Every write goes to a temporary file and then renames it into place, so an interrupted write never truncates a file.
- A keystroke marks the project dirty, and the next tick writes it, so typing never costs a file write.
- A field added to a record carries `#[serde(default)]`, so an older file still reads.
- A new enum variant is the exception. culm 0.1.1 refuses a project file that holds an `archived` session.

## Worktrees, tickets, and port numbers

culm never creates, changes, or removes a worktree. Claude Code makes them, in whatever layout the project's `CLAUDE.md` asks for, and registers them:

| Command | Effect |
| --- | --- |
| `culm session worktree add <path>` | `Git::worktree_info` reads the checkout that holds the path: its top, its repository name, and its branch. The top is recorded, so a path inside the worktree works too. Registering the same top again refreshes its branch. |
| `culm session worktree rm <path>` | Forgets the worktree. A worktree already gone from disk is forgotten by its path. |
| `culm session ticket <key>` | Records the ticket, trimmed. |
| `culm session port <0-9>` | Records the port number. Another session may hold the same number, and the reply names it. |
| `culm port list` | Each number from 0 to 9, and the sessions that hold it. An archived session gives its number back until it is restored. |

culm gives no meaning to a port number. The project's `CLAUDE.md` says which port each number stands for, and which numbers to avoid.

`culm session adopt` finds worktrees that sessions used before they registered anything. It reads each transcript twice: the `cwd` stamp of every line, which says where the session's shell stood, and the files the session wrote. A directory counts when `Git::worktree_info` names a linked worktree. A main checkout never counts, because every session shares it, and a worktree under the temporary directory is a scratch copy. The user picks the worktrees to register for each session, or `--yes` registers all of them. On 2026-10-24, on the maintainer's `spm` project, it read the transcripts of 28 sessions in 2.8 s with a debug build, and offered 27 worktrees to 14 of them.

## The changes panel

`Alt+Shift+G` opens a read-only panel that shows what the visible session changed. It never writes to a repository.

- **Tabs:** one tab per repository of the visible session. The worktrees the session registered come first. Every other repository the session wrote a file in follows, in the order the session first wrote in it. `transcript::edited_files` reads the written files from the transcript, and `Git::toplevel` names the repository that holds each one. The shell's panel shows the repositories registered with the project.
- **Shared repositories:** when the session registered at least one worktree, every other repository it wrote in is shared, such as a docs repository every session writes to. Its "Changes" list shows only the files this session wrote, and its count shows the changes left out, as `Changes (1+3)`. `a` shows every change, and `a` again goes back. A session that registered nothing sees every change, because nothing says which repository is its own.
- **Title:** the panel title names the ticket and the port number of the session it follows.
- **Lists:** "Changes" lists every file that differs from `HEAD`, from `git status --porcelain=v2`. "History" lists the last 200 commits of `HEAD`, and beside them the files of the picked commit, against its first parent.
- **Reads:** the panel reads the transcript and git when it opens, when the visible session changes, when the followed session sends `Stop`, when a command changes a session record, on `r`, and on `a`. No other event reads anything. A hook for another session reads nothing.
- **Diff:** `diff::side_by_side` lays the old version left and the new version right, three lines of context around each change. A removed line beside an added line is a pair, and the words that differ are marked. A file over 1 MiB or a binary file shows a notice instead.
- **Colors:** a thread computes the syntax colors of both versions, because coloring costs about 0.1 ms per line. The diff shows plain until the colors land. A file over 256 KiB gets no colors.
- **Size:** the panel opens on half of the room right of the sidebar, or over the whole session when `changes-maximized` is on. The panel's share and its list's share are dragged and saved per project. The list stays between 20 and 40 columns, and the history between 32 and 64.

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
- A transcript line that calls a tool holds a `tool_use` block with `name` and `input`. `Edit`, `MultiEdit`, and `Write` name their file in `input.file_path`, and `NotebookEdit` in `input.notebook_path`. A file a session changes through a shell command appears in no such block, so its repository gets no tab until the session also writes there through a tool.
- Claude Code breaks every line of its fullscreen renderer itself, with cursor moves, so `vt100` marks no row as wrapped. A check on 2026-10-26 found no wrapped row in 4,000 rows at 80, 120, and 200 columns. Claude Code 2.1.310 breaks a line one or two columns before the panel edge. A transcript line of type `assistant` holds each answer in a `text` part of `message.content`, and a line of type `user` holds a typed prompt as the string `message.content`.
- Claude Code asks for mouse reporting and paints its own viewport. Nothing scrolls off its screen, so `vt100` holds no scrollback for it.
- Claude Code on Linux reads byte `0x08` as `Ctrl+Backspace` and `ESC DEL` as `Meta+Backspace`. Both delete a word.
- Claude Code reads both `ESC [ H` and `ESC O H` as `Home`, and does not set DECCKM.

### About git

- `git` is on the `PATH`, and is version 2.31 or later, for `--diff-merges=first-parent`.
- Every git read of the changes panel runs on the interface thread. A slow repository stalls the frame for the time git takes.

### About the terminal

- The host terminal delivers `Alt` as a modifier or as an `ESC` prefix. A terminal that keeps `Alt` for itself takes away every culm shortcut.
- The terminal answers OSC 52, so a copy reaches the system clipboard. culm never reads the clipboard back. A middle click pastes culm's own last copy.
- The sixteen theme colors can repeat. gruvbox gives bright white and bright black the same value. A text color and its fill must never both come from the sixteen. The sidebar highlight uses color 237 of the 256-color ramp and the terminal's own foreground for that reason.

### About the machine

- One user runs culm on one machine. culm takes no file lock on the state files. Two windows on different projects are safe, because each project has its own state file.
- A socket that answers a connection belongs to a live culm. A socket file that refuses is left over from a crash, and culm replaces it.
- `culm hooks install` records the path of the binary. Moving the binary breaks every hook until `culm hooks install` runs again.
- `pactl` 16 or later exists, because `-f json` needs it. Without `pactl`, sessions start on the system default microphone.
- `/proc` exists, so memory sampling works. Memory sampling is the only Linux-only code path besides the release packages.

### About identifiers

- A slug is the identifier of a project and of a workspace. It names the state file, the socket, and the entries inside workspaces. A project rename moves the state file and rewrites every workspace that holds the project.
- A session slug never changes. Inside a project, `culm session --session` finds a session by its id, then its slug, then its name.
- A digit addresses a tab, and only a tab. Eight tabs and eight active sessions keep every count within one digit.

## Testing

Every side effect enters through a trait, and each trait has a fake in `src/testing.rs`:

| Trait | Real | Fake |
| --- | --- | --- |
| `PtySpawner` | `SystemPtySpawner` | `FakeSpawner` replays fixed output and records every write. |
| `Git` | `SystemGit` | `FakeGit` answers status, history, files, and worktree reads from what the test set. |
| `Store` | `FsStore` | `MemoryStore` keeps every file in memory and counts the writes. |
| `Clock` | `SystemClock` | `FakeClock` moves only when a test sets it. |
| `Recapper` | `ClaudeRecapper` | `FakeRecapper` answers after a set number of passes and counts kills. |
| `HookListener` | `SocketHookListener` | `FakeHookListener` hands the test a sender per project, delivers commands to the window, and answers the command line with a reply the test sets. |
| `MemoryProbe` | `ProcMemoryProbe` | `FakeMemoryProbe` returns set values. |
| `Namer` | `ClaudeNamer` | `FakeNamer` answers by prompt text. |
| `Microphones` | `PactlMicrophones` | `FakeMicrophones` holds one wanted source until a test sets others, or fails. |

`tests/session_lifecycle.rs` drives one project through `App`. `tests/browser.rs` drives the window through `Browser`, including the command line. `tests/import.rs` covers the import of existing transcripts. The tests start no process and write only inside a temporary directory.

No test covers the real terminal: rendering in kitty, the kitty keyboard protocol, OSC 52, and the real hook socket. The maintainer checks those by hand before a commit.
