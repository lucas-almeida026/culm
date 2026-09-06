# culm

Many Claude Code sessions in one project, as vertical tabs. One visible at a time. All of them running.

A grove of bamboo sends many culms up from one shared rhizome. Each stem stands on its own, and all of them are fed by the same root system.

## Status

Early. culm runs many sessions in one project, creates a git worktree per session per repository, marks a session that needs attention, holds a plain shell at position 0, and restores the active sessions on the next open. `cargo test` covers 100 cases through the fakes.

Not built yet: rename, archive, session reordering, and the TOML configuration file.

See [session-manager-spec.md](session-manager-spec.md) for the requirements, [FINDINGS.md](FINDINGS.md) for the spike that settled the stack, and [CLAUDE.md](CLAUDE.md) for the vision and the working rules.

## Use

```
culm hooks install                 # once, so attention markers work
culm project new <path>            # register a directory
culm project repo add <path>       # add a git repository to it
culm                               # open the project that owns this directory
culm project rm <project>          # unregister it; never deletes source or branches
```

Inside the interface: `Alt+0` focuses the shell, `Alt+<1-9>` focuses an active session, `Alt+Shift+N` creates one, `Alt+Shift+P` pauses or resumes the focused session, `Alt+Shift+X` deletes a paused one, `Alt+Shift+D` toggles nerd mode, `Ctrl+q` quits. A click focuses any row, and the separator drags to resize the sidebar.

## Build

```
cargo build --release
cargo test
```

Linux only. Other systems may work, and no test covers them.

## Contribution

Public domain under the [Unlicense](UNLICENSE). The project is closed to contributions. Fork it and make it yours.
