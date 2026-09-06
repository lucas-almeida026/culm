# Findings — embedding Claude Code sessions in a Rust TUI

Result of the spike defined in [SPIKE.md](SPIKE.md). Run on 2026-09-06. Feeds the build described in [../spm/session-manager-spec.md](../spm/session-manager-spec.md).

## Verdict

Pass on every criterion. A `ratatui` panel that hosts a `vt100` screen fed by `portable-pty` renders the full Claude Code interface, forwards every key that was tested, and stays two orders of magnitude inside the frame budget while it drains a flood from a session that is not focused. Build the real application on this stack.

## Proven versions

| Component | Version |
| --- | --- |
| rustc | 1.91.1 |
| ratatui | 0.30.2 |
| crossterm | 0.29, used through the `ratatui::crossterm` re-export |
| portable-pty | 0.9.0 |
| vt100 | 0.16.2 |
| tui-term | 0.3.4, default features |
| Claude Code | 2.1.263 |

The release binary is 1.3 MB. The whole spike is about 400 lines in `src/main.rs`.

## Measurements

Conditions: kitty, inside a tmux pane, 200x50 and 120x30, three sessions, one of which floods output continuously.

| Measurement | Value | Budget |
| --- | --- | --- |
| Frame render, Claude focused, flood running | p50 0.14 ms, p99 0.25 ms | p50 8 ms, p99 16 ms |
| Frame render, flood focused at 168x48 | p50 0.55 ms, p99 0.98 ms | 16 ms |
| Frame render, whole run | p50 0.51 ms, p99 0.77 ms | 16 ms |
| Key to first byte back | 2.85 ms idle, 7.35 ms under flood | 30 ms |
| Input to frame, p99 | 8.89 ms | 16 ms |
| Bytes drained from the unfocused flood | 12.8 GB | no stall |

One session is visible at a time by design, so these numbers cover the real workload rather than a simplified one. Render cost is not a limit at all: the worst case, a firehose in the visible panel, costs about 1 ms at p99 against a 16 ms budget. The limits that remain are the memory of each Claude process, measured at about 436 MB on 2026-09-02, and the cost of parsing output from sessions that are not visible. One session drained 12.8 GB while another was visible, and the visible panel still rendered at p50 0.14 ms.

## Architecture that worked

- One pseudoterminal per session, from `portable_pty::native_pty_system()`.
- One `vt100::Parser` per session, behind `Arc<Mutex<..>>`.
- One reader thread per session. The thread reads the pseudoterminal and feeds the parser.
- The main loop polls input with a 4 ms timeout, then draws at most every 8 ms.
- The main loop draws only the focused session.
- Panel geometry drives the pseudoterminal size. The child then wraps at the panel width.

Input is polled before the frame, so a keystroke never waits behind a render.

## API notes that cost time

1. **vt100 resize lives on the screen, not the parser.** Use `parser.screen_mut().set_size(rows, cols)`. `Parser::set_size` does not exist. This was the only compile error in the spike.
2. **Drop the pseudoterminal slave after spawning.** Call `drop(pair.slave)` right after `spawn_command`. Without the drop, the master never reaches end of file when the child exits.
3. **`tui-term` takes a screen reference.** `PseudoTerminal::new(&screen).block(..).cursor(..)`. The `vt100` feature is on by default. Version 0.3.4 builds against `ratatui-core` 0.1 and `ratatui-widgets` 0.3, which is the ratatui 0.30 family.
4. **Use the `ratatui::crossterm` re-export.** A separate `crossterm` dependency risks a version skew that breaks the backend.
5. **Drain every session, always.** The reader thread must run for sessions that are not focused. An undrained pseudoterminal fills its buffer and the child blocks. The symptom is a background session that appears to hang.

## Keyboard

`Alt+1` in legacy mode is two bytes, `ESC` and `1`. crossterm fuses them into one `Alt+1` event only when both bytes arrive in the same read. Under load the pair splits, the child receives a bare `ESC`, which Claude Code treats as an interrupt, and the digit lands in the prompt. The failure is intermittent.

Three defenses, in order:

1. Push `KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES` when `supports_keyboard_enhancement()` returns true. Modified keys then arrive as one CSI-u event.
2. Offer function keys as a second binding. One function key is one escape sequence, so it never depends on fusion.
3. Offer a mouse click, which does not use the keyboard at all.

Note that a query for keyboard enhancement returns false inside tmux, so the spike ran on the legacy path. **The kitty protocol path is unverified.** Run the binary directly in kitty to confirm the indicator in the sidebar turns green.

Verified key translations, from the focused panel to the child:

| Key | Bytes |
| --- | --- |
| `shift+tab` (`KeyCode::BackTab`) | `ESC [ Z` |
| `Ctrl` plus a letter | the letter masked with `0x1f` |
| `Enter` | `\r` |
| `Backspace` | `0x7f` |
| `Esc` | `0x1b` |
| Arrows, plain | `ESC [ A` through `ESC [ D` |
| Arrows, modified | `ESC [ 1 ; <m> <letter>` |
| Paste | the text wrapped in `ESC [ 200 ~` and `ESC [ 201 ~` |

A three-line paste arrived in the prompt as one block, and not as three submissions.

Reserved keys must stay few, because everything else belongs to the child. `Ctrl+q`, `Alt` plus a digit, and `F1` through `F9` produced no conflict with Claude Code in this run.

## Mouse

`EnableMouseCapture` plus SGR events. The sidebar hit box is recomputed on every draw, so a click stays correct after a resize. A click on a session row switches to that session. A click inside the terminal panel is ignored.

Mouse capture takes selection away from the host terminal while the application runs. In kitty, `shift` plus drag still selects. Forwarding mouse events to the child needs the child's mouse mode, which this spike does not implement.

## Consequences for the real application

- The stack is settled. Build on ratatui, portable-pty, and a vt100 screen.
- `tui-term` with the vt100 backend was enough. Move to `wezterm-term` or `alacritty_terminal` only for images, because vt100 models no image protocol.
- Draw only the focused session. Parse every session. This is the whole performance strategy, and it is what keeps a background firehose cheap.
- Keep the reserved key set small, and show the user which keyboard mode is active.
- The daemon and client split from the specification is still the right shape for pause and restore. This spike runs as one process, so it does not test that.
- Markers reach the interface through hooks. Without tmux, identify a session with an injected environment variable plus `CLAUDE_CODE_SESSION_ID`, and let the hook post to the daemon socket. Hook processes inherit the environment, which was verified on 2026-09-02.

## Not verified

- The kitty keyboard protocol path.
- Mouse events forwarded to the child.
- A child that exits or dies while the interface runs.
- Scrollback navigation inside a panel.
- Images in a panel.
- Pause, save, and restore across a restart.
- Any terminal other than kitty, and any platform other than Linux.
