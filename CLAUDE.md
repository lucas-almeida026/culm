# culm

A grove of bamboo sends many culms up from one shared rhizome. Each stem stands on its own. All of them are fed by the same root system.

## What this is

culm is a terminal application. It runs many Claude Code sessions inside one project and shows them as vertical tabs. One session is visible at a time. Every session keeps running whether or not it is visible.

## Why it exists

The author works on several features per week, and uses one Claude Code session per feature. Those sessions live across seven repositories that belong to one project. Three problems drove this project:

1. **Sessions are hard to reach.** A parked session is a UUID in `~/.claude/projects/`. Finding the right one means reading transcripts.
2. **A terminal multiplexer is the wrong shape.** tmux keeps sessions alive, but it manages windows, not projects. It knows nothing about which session needs attention, which repository a session belongs to, or how to resume a transcript.
3. **Existing tools bind a session to one repository.** claude-squad requires a git repository at the working directory and gives each session a git worktree. A project that spans seven repositories does not fit that model, and neither does a session that edits two repositories at once.

culm treats the project as the unit. A project is any directory. Repositories join a project, including repositories outside the project directory. A session reaches every repository in its project.

## What it does

- Runs many sessions at once. Shows one. Keeps parsing the rest, because an undrained pseudoterminal blocks its child.
- Renders the focused session as a full interactive terminal panel. Every key reaches the child unchanged, including `shift+tab`, `Ctrl+r`, `Esc`, and a bracketed paste.
- Resumes an existing session with `claude -r`, which reloads the whole transcript.
- Marks a session that needs attention, from Claude Code hooks: needs permission, needs an answer, or finished. A marker clears when the user resolves the issue, and never when the user merely looks at the session.
- Creates a git worktree and a branch for a session that edits a repository, so that two sessions editing one repository do not collide.
- Saves state on close and restores it on open, so that a reboot costs nothing.

Full requirements live in `./session-manager-spec.md`. Evidence for the architecture lives in [FINDINGS.md](FINDINGS.md), which records a spike that proved the stack on 2026-09-06.

## Not in scope

- Git diff, commit, and push. Worktree and branch management is the only version control work.
- Automatic acceptance of permission prompts.
- Any change to where the Claude Code CLI writes transcripts.
- Any target other than Linux. Other systems may work. No test covers them.

## Contribution

The source is public domain under the Unlicense. The project is closed to contributions. Fork it and make it yours.

---

# How to work in this repository

## Order of work

Make it work. Make it right. Make it fast. In that order, and never skip forward.

1. **Make it work.** Write the smallest version that produces the real behavior end to end. A skeleton that runs beats a design that does not.
2. **Make it right.** Add the tests, name things properly, push side effects to the edges, and delete what the working version proved unnecessary.
3. **Make it fast.** Only after a measurement shows a real cost. `FINDINGS.md` holds the budgets: frame render p99 under 16 ms, key to echo under 30 ms. Do not optimize without a number.

A commit that jumps to step 3 without a measurement is a defect.

## Rust practice

- Target Rust edition 2024. Format with `cargo fmt`. The repository holds `rustfmt.toml`.
- `unsafe` is forbidden. `Cargo.toml` sets `unsafe_code = "forbid"` under `[lints.rust]`, so the compiler rejects it.
- Library code does not call `unwrap` or `expect`. `Cargo.toml` sets both to `warn` under `[lints.clippy]`. Return `Result` and let the caller decide. Test code may use `expect` with a message, and each test file states that exemption at the top.
- Return `anyhow::Result` at the application boundary. Introduce a typed error only when a caller needs to branch on the failure.
- Keep `src/main.rs` thin. Logic lives in the library, because an integration test under `tests/` reaches the library and never the binary.
- Prefer `&str` and slices in signatures. Take ownership only when the value is stored.
- Write identifiers in English. Write comments that explain why, not what. Do not narrate a change in a comment, because git already holds that history.
- Document a public item when its purpose is not obvious from its name.

## Dependency injection

Every side effect enters through a trait. This is what makes the tests fast and deterministic.

- `PtySpawner` in `src/pty.rs` is the process boundary. `SystemPtySpawner` starts a real child. `FakeSpawner` in `src/testing.rs` replays canned bytes and records what was written.
- A type receives its dependencies through its constructor or through the call. `App::add_session` takes `&dyn PtySpawner` for that reason.
- No global state, no singleton, no lazily initialized handle to the operating system.
- Core logic never calls `std::process`, `std::fs`, or `std::time::SystemTime` directly. When a new side effect appears, add a trait for it, add a fake, and inject both.
- Prefer a generic parameter when the type is known at compile time. Use `&dyn Trait` when a collection holds several implementations.

## Tests

- Run `cargo test` before every commit. Run `cargo clippy --all-targets` as well.
- Put unit tests beside the code in a `#[cfg(test)] mod tests`. Put behavior that crosses modules in `tests/`.
- Name a test after the behavior it protects. `keys_reach_the_visible_session_only` states a rule. `test_keys` states nothing.
- Use fakes, not mocks. Assert on the result, not on the sequence of calls.
- Never sleep for a fixed period. Poll for the condition with a timeout, as `Session::wait_for_text` does.
- A test never starts a real process, and never writes outside a temporary directory.
- Every fixed bug gets a test that fails without the fix.
- Terminal rendering that needs a real terminal is tested by hand. Record what was checked in `FINDINGS.md`, and mark it as manual.

## Commits

Write one line in conventional commit form. No body. For example, `feat: switch sessions with a sidebar click`.

## Layout

| Path | Holds |
| --- | --- |
| `src/main.rs` | Terminal setup, the event loop, and teardown. Nothing else. |
| `src/lib.rs` | The module list. |
| `src/app.rs` | Application state, focus, and key dispatch. No input and no output. |
| `src/session.rs` | One session: its child, its screen, and the thread that drains it. |
| `src/pty.rs` | The process boundary. `SessionSpec`, `Pty`, and `PtySpawner`. |
| `src/keys.rs` | Key translation. Pure functions, fully unit tested. |
| `src/ui.rs` | Rendering, and the hit box that maps a click back to a session. |
| `src/testing.rs` | Test doubles, compiled into the library so `tests/` can use them. |
| `tests/` | Behavior that crosses modules, driven through the fakes. |

## Traps already paid for

These cost time once. `FINDINGS.md` holds the detail.

- `vt100` resize lives on the screen. Call `parser.screen_mut().set_size(rows, cols)`.
- Drop the pseudoterminal slave right after spawning, or the master never reaches end of file.
- Drain every session, always. An undrained pseudoterminal fills and the child blocks. The symptom looks like a hung background session.
- Use the `ratatui::crossterm` re-export. A separate `crossterm` dependency risks a version skew.
