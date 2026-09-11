# culm

Many Claude Code sessions in one project, as vertical tabs. One visible at a time. All of them running.

A grove of bamboo sends many culms up from one shared rhizome. Each stem stands on its own, and all of them are fed by the same root system.

## Why it exists

The author works on several features per week and uses one Claude Code session per feature. Those sessions live across seven repositories that belong to one project. Three problems drove this:

**Sessions are hard to reach.**

**A terminal multiplexer is the wrong shape.** tmux keeps sessions alive, but it manages windows, not projects. It knows nothing about which session needs attention, which repository a session belongs to, or how to resume a transcript.

**Existing tools bind a session to one repository.** [claude-squad](https://github.com/smtg-ai/claude-squad) requires a git repository at the working directory and gives each session a worktree. A project that spans seven repositories does not fit that model, and neither does a session that edits two repositories at once.

culm treats the project as the unit. A project is any directory. Repositories join a project, including repositories outside the project directory. A session reaches every repository in its project.

## How it works

**One process, no daemon.** Every session is a real child process on its own pseudoterminal, opened with `portable-pty`. culm owns every one of them. Nothing survives the interface exiting, and nothing else supervises them.

**Every session is drained, visible or not.** Each session runs a thread that reads its pseudoterminal and feeds a `vt100` parser holding two thousand lines of scrollback. This is not an optimization: an undrained pseudoterminal fills its buffer and the child blocks, which looks exactly like a hung background session. Drawing a frame copies the focused session's screen into a ratatui buffer, so switching tabs is a change of which screen gets copied and costs nothing.

**The loop reads input first.** A keystroke never waits behind a frame. Hook messages come next, then a once-a-second pass that samples memory and reaps a child that ended, then a redraw capped at one per eight milliseconds so a session printing at full speed cannot drive the render.

**Transcripts stay where Claude Code puts them.** culm generates a UUID and passes `--session-id` the first time a session starts, then `--resume <uuid>` every time after, which reloads the whole conversation. The transcript itself lives at `~/.claude/projects/<cwd with every / and . turned into ->/<uuid>.jsonl`, written by the CLI. culm reads those files to import a session and deletes one when you delete its session, and never moves or rewrites them.

**Pausing is a signal, not a save.** A pause sends SIGTERM to the child and keeps the session id. Resuming spawns a fresh `claude --resume` against the same id, so the conversation comes back and the memory does not stay held.

**Attention markers arrive over a socket.** `culm hooks install` adds entries to `~/.claude/settings.json` that point nine hook events back at the culm binary. Each session culm starts carries `CULM_SOCKET` in its environment; when a hook fires, the short-lived `culm hook` process posts one JSON payload to that Unix socket and the interface marks the matching session. A `claude` you start yourself has no `CULM_SOCKET`, so its hooks exit quietly. culm never reads a session's terminal output to decide anything.

One gap is worth knowing: no hook reports that you answered a permission prompt, so `🔐` would otherwise stay up for the whole time the approved tool runs. The keystroke that answers the prompt is the only signal culm gets, so that keystroke clears the marker. Merely looking at a session never clears one.

**Worktrees are the only version control work.** A session that edits a repository gets `git worktree add -b <session-slug>` under `<project root>/.worktrees/<repo>-<session>`, so two sessions editing one repository do not collide. culm creates worktrees and removes them. It never commits, never pushes, and never deletes a branch.

**State is two JSON files.** A global `registry.json` lists every project, and `projects/<slug>.json` holds one project's root, repositories, and session records, both under `$XDG_STATE_HOME/culm` (or `~/.local/state/culm`). A record is a session id, a name, its repositories, and when it was last used — never the conversation, which is the transcript's job. Every write goes to a temporary file and is renamed into place, so an interrupted write cannot truncate saved state.

**The shell at position 0 is the same machinery.** It spawns `$SHELL` at the project root through the same pseudoterminal code, minus the session id, the marker, the worktree, and `CULM_SOCKET`. It is not saved, it does not count toward the nine-session limit, and it restarts on the next tick if you exit it.

**Keys go through one translation table.** culm asks the terminal for the kitty keyboard protocol and uses it when the answer is yes, because it reports a modified key as a single unambiguous event. A terminal that says no runs the legacy path instead; the protocol is an enhancement and never a requirement. The bottom bar names which mode you got.

**Every side effect sits behind a trait.** Spawning a process, running git, reading files, reading memory, telling the time, and naming an imported session each have one trait and one fake. Nothing in the core calls `std::process`, `std::fs`, or the clock directly, which is why `cargo test` covers 250 cases without starting a process, touching disk outside a temporary directory, or sleeping.

## What it is

One Rust binary, for Linux. Early, and in use daily. The TOML configuration file is the one planned piece not built yet.

Register a project and open it:

```
culm hooks install                 # once, so attention markers work
culm project new <path>            # register a directory, named after its last part
culm project new <path> --name <name>
                                   # register it under a name you choose
culm project alter name <name>     # rename it, so culm open <name> works
culm project new <path> --import-native-sessions
                                   # register it and pull in the sessions it holds
culm project repo add <path>       # add a git repository to it
culm project repo list             # show the repositories it holds
culm project import                # pull in sessions that appeared since
culm                               # open the project that owns this directory
culm open --import-native-sessions # import, then open
culm project rm <project>          # unregister it, never deletes source or branches
```

A project name becomes a slug, so `--name "Billing Rewrite"` gives you `culm open billing-rewrite`. A name another project already holds is refused rather than numbered. A rename moves the saved state and keeps the root, the repositories, and every session.

An import keeps the session id of each transcript, so resuming one reaches the same conversation. It takes the name from the title the session carries inside Claude Code. A session that was never named there is named by a headless `claude -p --model haiku` child reading the first prompt. Every imported session arrives paused.

Inside the interface, `Alt+Shift+H` shows the table of every binding. The bottom bar points at it against the left edge at all times, names the keyboard mode the host terminal gave culm, and puts whatever last happened against the right edge. In short: `Alt+0` focuses the shell, `Alt+<1-9>` focuses an active session, `Alt+Shift+N` creates one, `Alt+Shift+P` pauses or resumes the focused session, `Alt+Shift+R` renames it, `Alt+Shift+F` searches the paused list, `Alt+Shift+X` deletes a paused one after a yes or no confirmation, `Alt+Shift+D` toggles nerd mode, `Ctrl+q` quits. A click focuses any row, and the separator drags to resize the sidebar.

The paused list is ordered by last use, newest first. `Alt+Shift+F` puts the cursor in the search box above it, which filters by name on every keystroke. `Enter` lands on the first match, and `Esc` clears the filter.

Drag over a panel to select, and the text reaches the system clipboard when the button comes up. A middle click pastes the last copy into the focused session. An alt click extends the selection to a new point, keeping the anchor, so a block longer than the panel is copied in two gestures. Alt rather than shift, because a terminal keeps shift and the mouse for its own selection even while an application holds the mouse.

Scrolling keeps the selection on the text it marks. When culm owns the scrollback it knows exactly how far the view went. A session that scrolls itself, such as Claude Code, repaints its own panel instead and reports nothing, so culm hashes the rows before the notch and lines them up against the rows that arrive after, then moves the marks by that distance. A repaint that does not line up was a new screen rather than a scroll, and the selection is dropped. Such a session keeps its own history, so text that scrolls off its panel is gone as far as culm is concerned and cannot be copied back.

The mouse wheel over a panel scrolls. A session that handles the mouse itself, such as Claude Code, receives the notch and scrolls its own conversation. For a plain shell, culm scrolls its own buffer instead, `Shift+PageUp` and `Shift+PageDown` move half a panel, and typing returns the view to the live output.

## Build

```
cargo build --release
cargo test
```

Linux only. Other systems may work, and no test covers them.

The requirements live in [session-manager-spec.md](session-manager-spec.md), and the working rules live in [CLAUDE.md](CLAUDE.md).

## Contribution

Public domain under the [Unlicense](UNLICENSE). The project is closed to contributions. Fork it and make it yours.
