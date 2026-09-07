# culm

Many Claude Code sessions in one project, as vertical tabs. One visible at a time. All of them running.

A grove of bamboo sends many culms up from one shared rhizome. Each stem stands on its own, and all of them are fed by the same root system.

## Status

Early. culm runs many sessions in one project, creates a git worktree per session per repository, marks a session that needs attention, holds a plain shell at position 0, imports the Claude Code sessions a directory already holds, and restores the active sessions on the next open. `cargo test` covers 211 cases through the fakes.

Not built yet: archive, session reordering, and the TOML configuration file.

See [session-manager-spec.md](session-manager-spec.md) for the requirements, [FINDINGS.md](FINDINGS.md) for the spike that settled the stack, and [CLAUDE.md](CLAUDE.md) for the vision and the working rules.

## Use

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

Inside the interface: `Alt+0` focuses the shell, `Alt+<1-9>` focuses an active session, `Alt+Shift+N` creates one, `Alt+Shift+P` pauses or resumes the focused session, `Alt+Shift+R` renames it, `Alt+Shift+F` searches the paused list, `Alt+Shift+X` deletes a paused one after a yes or no confirmation, `Alt+Shift+D` toggles nerd mode, `Ctrl+q` quits. A click focuses any row, and the separator drags to resize the sidebar.

The paused list is ordered by last use, newest first. `Alt+Shift+F` puts the cursor in the search box above it, which filters by name on every keystroke. `Enter` lands on the first match, and `Esc` clears the filter.

Drag over a panel to select, and the text reaches the system clipboard when the button comes up. A middle click pastes the last copy into the focused session.

The mouse wheel over a panel scrolls. A session that handles the mouse itself, such as Claude Code, receives the notch and scrolls its own conversation. For a plain shell, culm scrolls its own buffer instead, `Shift+PageUp` and `Shift+PageDown` move half a panel, and typing returns the view to the live output.

## Build

```
cargo build --release
cargo test
```

Linux only. Other systems may work, and no test covers them.

## Contribution

Public domain under the [Unlicense](UNLICENSE). The project is closed to contributions. Fork it and make it yours.
