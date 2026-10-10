# culm

A grove of bamboo sends many culms up from one shared rhizome. Each stem stands on its own. All of them are fed by the same root system.

culm is a terminal application. It opens up to eight projects in one window, as tabs along the top. Each project runs many Claude Code sessions and lists them in its own sidebar. One session is visible at a time. Every session keeps running whether or not it is visible.

[ARCHITECTURE.md](ARCHITECTURE.md) explains how the parts fit together, and the assumptions the code makes without saying so. Read it before a change that crosses modules.

## Vision

culm is the place where a developer runs every Claude Code session of the week, and never loses one.

The author works on several features per week, with one Claude Code session per feature, across many repositories. culm exists for that workflow first. The source is public so that others can fork it. A request from outside never changes the direction.

## Goals

These goals decide between two designs. A change that serves none of them needs a reason.

1. **Zero-cost context switching.** A switch between features, sessions, and projects costs nothing. No state is lost, no transcript is hunted for, and a reboot costs only the restart.
2. **Attention, not polling.** The user never checks a session to learn whether it needs them. culm marks the session and its tab.
3. **TUI over GUI.** culm stays a terminal application. A feature that needs a graphical window is out of scope.
4. **Full CLI compatibility.** This goal has two halves:
   - Claude Code works unchanged inside a session. Every key, paste, mouse event, slash command, and model switch behaves as it does in a plain terminal.
   - culm is fully scriptable. Every operation on projects, repositories, workspaces, and settings is reachable from the culm command line, not only from the interface.
5. **Fast enough to forget.** Frame render p99 stays under 16 ms. Key to echo stays under 30 ms. These two numbers are the only performance targets.

## Road to v1.0

culm stays below v1.0 until every item below holds:

1. **Stable saved state.** The state files keep a format that culm promises to read forever. A format change ships with a migration, never with a break.
2. **Configurable keys.** Every binding lives in the configuration file, and one binding is replaceable without restating the rest.
3. **Crash survival.** Sessions survive an unexpected exit of the interface.
4. **Daily-driver complete.** The author's daily workflow needs no further change to culm.
5. **Terminal support.** culm works in common terminals other than kitty.
6. **Planned providers.** A plan exists for agent CLIs other than Claude Code. Before v1.0, culm supports Claude Code only.
7. **Planned platforms.** A plan exists for other systems, in this order: other Linux distributions, then macOS, then Windows. Windows is a distant option, not a goal.

## Platforms

culm targets Debian-based Linux distributions. The release ships a `.deb` and a static musl binary. Other systems may work. No test and no release covers them.

## Not in scope

- Git diff, commit, and push. Worktree and branch management is the only version control work.
- Automatic acceptance of permission prompts.
- Any change to where the Claude Code CLI writes transcripts.
- A graphical interface.
- Agent CLIs other than Claude Code, before v1.0.

## Decisions, and why

| Decision | Why |
| --- | --- |
| The project is the unit, and a project is any directory. | The author's projects span many repositories. A tool that binds a session to one git repository does not fit. |
| A session reaches every repository of its project. | One feature often edits two repositories at once. |
| One git worktree and one branch per session per repository. | Two sessions that edit one repository must not collide. |
| One window holds up to eight projects as tabs. | A browser is the familiar model for many open things, one visible. Eight keeps one digit per tab. |
| One process, no daemon. | One process is simple to reason about and to test. Crash survival is deferred, not refused. It is a v1.0 item. |
| culm owns every pseudoterminal and parses every session with `vt100`. | culm must render a session inside its own layout, and keep every session drained. |
| Attention markers come from Claude Code hooks over a Unix socket. | Reading terminal output to guess a state is fragile. Hooks report the state directly. |
| Sessions resume with `claude --resume`. | Claude Code keeps the transcript. culm keeps only the session id, so no conversation is duplicated. |
| State lives in small JSON files under the XDG state directory. | The files are easy to read, to repair by hand, and to write atomically. |
| `Alt` is the only leader key. No function key is reserved. | Every reserved key is a key Claude Code loses. |
| Workspaces name sets of projects. | Reopening the same set of projects every day must cost one command. |
| The project is closed to contributions. | culm serves one workflow. A fork serves another. |

## How to work in this repository

### Order of work

Make it work. Make it right. Make it fast. In that order, and never skip forward.

1. **Make it work.** Write the smallest version that produces the real behavior end to end. A skeleton that runs beats a design that does not.
2. **Make it right.** Add the tests, name things properly, push side effects to the edges, and delete what the working version proved unnecessary.
3. **Make it fast.** Only after a measurement shows a real cost against the budgets in the goals. Do not optimize without a number.

A commit that jumps to step 3 without a measurement is a defect.

### Branches

- Develop on a branch named after the next version, such as `v0.3.0`. The project runs no parallel development, so use no worktree for culm itself.
- A branch name equals the tag name of its release. Push a tag by its full ref name, `refs/tags/vX.Y.Z`, and delete a branch by `refs/heads/vX.Y.Z`. A short name is ambiguous, and git refuses it.
- Commit and push only when the maintainer approves the change.

### Rust practice

- Target Rust edition 2024. Format with `cargo fmt`. The repository holds `rustfmt.toml`.
- `unsafe` is forbidden. `Cargo.toml` sets `unsafe_code = "forbid"` under `[lints.rust]`, so the compiler rejects it.
- Library code does not call `unwrap` or `expect`. `Cargo.toml` sets both to `warn` under `[lints.clippy]`. Return `Result` and let the caller decide. Test code may use `expect` with a message, and each test file states that exemption at the top.
- Return `anyhow::Result` at the application boundary. Introduce a typed error only when a caller needs to branch on the failure.
- Keep `src/main.rs` thin. Logic lives in the library, because an integration test under `tests/` reaches the library and never the binary.
- Prefer `&str` and slices in signatures. Take ownership only when the value is stored.
- Write identifiers in English. Write comments that explain why, not what. Do not narrate a change in a comment, because git already holds that history.
- Document a public item when its purpose is not obvious from its name.

### Dependency injection

Every side effect enters through a trait. The tests are fast and deterministic because of this rule.

- `Deps` in `src/app.rs` carries every side effect a project needs: `PtySpawner`, `Git`, `Store`, `Clock`, `Recapper`, and `HookListener`. Pass `Deps` at the call. Never store it.
- Each trait has a real implementation and a fake in `src/testing.rs`.
- No global state, no singleton, no lazily initialized handle to the operating system.
- Core logic never calls `std::process`, `std::fs`, or `std::time::SystemTime` directly. When a new side effect appears, add a trait for it, add a fake, and inject both.
- Prefer a generic parameter when the type is known at compile time. Use `&dyn Trait` when a collection holds several implementations.

### Tests

- Run `cargo test` and `cargo clippy --all-targets` before every commit.
- Put unit tests beside the code in a `#[cfg(test)] mod tests`. Put behavior that crosses modules in `tests/`.
- Name a test after the behavior it protects. `keys_reach_the_visible_session_only` states a rule. `test_keys` states nothing.
- Use fakes, not mocks. Assert on the result, not on the sequence of calls.
- Never sleep for a fixed period. Poll for the condition with a timeout, as `Session::wait_for_text` does.
- A test never starts a real process, and never writes outside a temporary directory.
- Every fixed bug gets a test that fails without the fix.

### Test by hand

Behavior that needs a real terminal is tested by hand, by the maintainer.

1. Close the running culm.
2. Run `cargo build --release` in the repository.
3. Start culm again, and check the change.

Never test a new build inside a running culm. The older culm can mask or distort the result.

### Commits

Write one line in conventional commit form. No body. For example, `feat: switch sessions with a sidebar click`.

### Human-facing documents

Write `ARCHITECTURE.md`, `CHANGELOG.md`, release notes, and this file in Simplified Technical English: short active sentences, present tense, absolute dates, and evidence for every claim. `README.md` is the exception. It is a pitch and a quick start, so it explains no decision and no internal design.

### Versions

- Follow semantic versioning. A feature raises the minor version. A fix raises the patch version.
- Record every change under `[Unreleased]` in `CHANGELOG.md`, in the commit that makes the change.
- `Cargo.toml` holds the version. Raise it only in the release commit.

### Release

Follow these steps for version `X.Y.Z`, on the branch `vX.Y.Z`:

1. Set `version` in `Cargo.toml` to `X.Y.Z`, and run `cargo build` to update `Cargo.lock`.
2. Rename `[Unreleased]` in `CHANGELOG.md` to `[X.Y.Z] - <date>`, and add its compare link.
3. Commit `chore: release X.Y.Z`. Push the branch.
4. Fast-forward `main`: `git checkout main`, `git merge --ff-only vX.Y.Z`, `git push origin main`.
5. Tag the commit: `git tag -a vX.Y.Z -m "culm vX.Y.Z"`, then `git push origin refs/tags/vX.Y.Z`.
6. Build the binary: `cargo build --release --target x86_64-unknown-linux-musl`. Copy it out of `target/` and run `strip` on the copy.
7. Make the tarball. Put the binary, `README.md`, and `UNLICENSE` in a directory named `culm-vX.Y.Z-x86_64-unknown-linux-musl`, then run `tar czf` on that directory.
8. Make the `.deb` by hand with `dpkg-deb`:
   1. Unpack the previous release's `.deb` with `dpkg-deb -R`, to reuse its `DEBIAN/control` and `usr/share/doc/culm/copyright`.
   2. Lay out `usr/bin/culm`, `usr/share/doc/culm/README.md.gz` (from `gzip -9n`), and the copyright file.
   3. Set `Version: X.Y.Z-1` and `Installed-Size` (from `du -sk`) in `DEBIAN/control`.
   4. Run `dpkg-deb --root-owner-group -Zxz --build <dir> culm_X.Y.Z-1_amd64.deb`.
9. Write one `.sha256` file per asset with `sha256sum`.
10. Write the release notes in Simplified Technical English, in the layout of the previous release: install, the binary with its checksums, added, fixed, and known limits.
11. Publish: `gh release create vX.Y.Z --verify-tag --title "culm vX.Y.Z" --notes-file <notes> --latest <assets>`.
12. Download the published assets with `gh release download`, and run `sha256sum -c` on them.
13. Delete the merged branch: `git branch -d vX.Y.Z` and `git push origin --delete refs/heads/vX.Y.Z`. Create the branch of the next version from `main`.

Do not run `cargo build --release` while the maintainer uses culm from `target/release/culm`, unless the maintainer asks. That build replaces the binary the maintainer runs.

## Traps already paid for

These cost time once. `ARCHITECTURE.md` explains each one.

- `vt100` resize lives on the screen. Call `parser.screen_mut().set_size(rows, cols)`.
- Drop the pseudoterminal slave right after spawning, or the master never reaches end of file.
- Drain every session, always. An undrained pseudoterminal fills and the child blocks. The symptom looks like a hung background session.
- Use the `ratatui::crossterm` re-export. A separate `crossterm` dependency risks a version skew.
- Never draw text in one of the sixteen theme colors on a fill in another. A theme may give both the same value, and gruvbox does.
