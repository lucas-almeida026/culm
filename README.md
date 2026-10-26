# culm

**Every Claude Code session you run, in one window.** Open your projects as tabs, run many sessions in each, and see at a glance which one needs you.

A grove of bamboo sends many culms up from one shared rhizome. Each stem stands on its own, and all of them are fed by the same root system.

- **Projects as tabs.** Up to eight projects in one window, like a browser. A workspace reopens the whole set with one command.
- **Many sessions per project.** One session per feature, all running at once, one on screen. Each session is a real, fully interactive Claude Code terminal.
- **Know who needs you.** 🔐 needs a permission, ❓ needs an answer, ✅ finished. Every session shows its marker, and every tab shows the most urgent one inside it.
- **Nothing gets lost.** Close culm, reboot, come back: every session resumes its whole conversation.
- **Built for multi-repository work.** A project is any directory, and a session reaches every repository under it. Claude Code makes the worktrees a feature needs, and tells culm which ones belong to its session, together with the ticket and a port number. culm shows them and never touches your git.

## Install

Requirements: Linux, `git`, and the [Claude Code CLI](https://docs.claude.com/en/docs/claude-code).

Download the latest release from the [releases page](https://github.com/lucas-almeida026/culm/releases/latest).

On Debian or Ubuntu:

```
sudo apt install ./culm_<version>-1_amd64.deb
```

On any other distribution:

```
tar xzf culm-v<version>-x86_64-unknown-linux-musl.tar.gz
install -m 755 culm-v<version>-x86_64-unknown-linux-musl/culm ~/.local/bin/culm
```

Then turn on the attention markers, once:

```
culm hooks install
```

## Quick start

1. Register a project. Any directory works:

   ```
   culm project new ~/work/billing
   ```

2. Optionally, add the repositories the project's shell shows changes for:

   ```
   culm project repo add ~/work/billing/api
   ```

3. Already have Claude Code sessions in that directory? Bring them in:

   ```
   culm project import
   ```

4. Open it:

   ```
   cd ~/work/billing && culm
   ```

5. Press `Alt+Shift+N`, name the session, and press `Enter`. Repeat for every feature you work on.

Press `Alt+Shift+H` inside culm at any time for every shortcut.

## Work on several projects

Open more than one project, one tab each:

```
culm open billing crm
```

Save a set you open often as a workspace, and reopen it in one command:

```
culm workspace new daily billing crm
culm open -w daily
```

Opened several projects without a workspace? culm offers to save them as one when you quit.

## Shortcuts

| Keys | Does |
| --- | --- |
| `Alt+1` to `Alt+8` | Switch project tab |
| `Alt+Shift+T` / `Alt+Shift+W` | Open a project tab / close this tab |
| `Alt+Shift+←` / `Alt+Shift+→` | Move this tab left / right (or drag it) |
| `Alt+Shift+N` | New session |
| `Alt+Shift+P` | Pause or resume the session |
| `Alt+Shift+R` | Rename the session |
| `Alt+Shift+A` / `Alt+Shift+L` | Archive the session / list the archive |
| `Alt+Shift+X` | Delete a paused session, or an archived one from the archive list |
| `Alt+Shift+F` / `/` in the sidebar | Search paused sessions |
| `Alt+Shift+G` | Show or hide the changes of the session |
| `Alt+Shift+O` | Move the keys between the sidebar, the session, and the changes |
| `j` `k` / `Enter` | In the sidebar: move / open the session |
| `J` `K` | In the sidebar: reorder sessions |
| `Alt+0` | The project's shell |
| `Alt+Shift+S` | Settings |
| `PageUp` / `PageDown` | Scroll the session half a panel |
| `Ctrl+Enter` | Claude Code's "send now" |
| `Alt+Shift+Z` | Take back the last paste in a Claude Code session |
| `Ctrl+z` | Kept from Claude Code, which would suspend itself. `culm config set block-ctrl-z false` sends it |
| `Ctrl+q` | Quit |

The mouse works too: click a tab or a session, drag to select and copy, middle click to paste, and scroll with the wheel.

## Tips

- **Pause what you are not using.** A paused session frees its memory and resumes with its whole conversation. A project runs up to eight sessions at a time.
- **Get a recap when you come back.** Run `culm config set recap-on-pause true`, and a paused session tells you what it was doing.
- **Archive the done work.** Archived sessions leave the sidebar but stay one keystroke away in the archive list.
- **See what a session changed.** `Alt+Shift+G` opens the diff of every repository the session wrote in, old beside new. Run `culm config set changes-maximized true` to open it over the whole session.
- **Tell culm where a session works.** Inside a session, `culm session worktree add <path>`, `culm session ticket <key>`, and `culm session port <0-9>` record its worktrees, its ticket, and its port number. The changes panel opens on those worktrees, and the sidebar shows the port. Put the commands in your project's `CLAUDE.md`, and Claude Code runs them itself.
- **Copy answers with their formatting.** Run `culm config set copy-markdown true`, and a copy of a Claude Code answer comes back as the markdown Claude wrote, with whole lines instead of the breaks the panel drew.
- **Already have worktrees?** `culm session adopt` finds the worktrees each session worked in, from its conversation, and offers to record them.

## Command line

```
culm project list                       # every registered project
culm project alter name <name>          # rename the project in this directory
culm project rm <project>               # unregister it, never deletes source or branches
culm workspace list                     # every workspace and its projects
culm workspace add <name> <project>     # add a tab to a workspace
culm workspace remove <name> <project>  # take a tab out of a workspace
culm config list                        # settings for every project
culm session show                       # what this session recorded
culm session list                       # every session with its ticket and port
culm port list                          # which port numbers are free
```

`culm --help` and `culm <command> --help` list the rest.

## Build from source

```
cargo build --release
cargo test
```

## License

Public domain, under the [Unlicense](UNLICENSE). The project is closed to contributions. Fork it and make it yours.
