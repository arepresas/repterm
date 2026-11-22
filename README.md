# repterm

A fast, simple terminal emulator built with Rust, egui, and egui_term.

## Features

- Multiple tabs with keyboard and mouse controls
- Git and GitHub status in a bar below the tabs
- OSC 52 clipboard support
- Configurable shell via the `SHELL` environment variable

## Controls

| Action | Shortcut |
| --- | --- |
| New tab | `Ctrl+T` |
| Close tab | `Ctrl+W`, `Ctrl+Delete`, middle-click, or the `×` button |
| Previous tab | `Ctrl+PageUp` |
| Next tab | `Ctrl+PageDown` |
| Delete word | `Ctrl+Backspace` |

## Git and GitHub

The status bar reads the active tab's working directory and shows the current
branch, dirty state, and ahead/behind counts. If a GitHub pull request exists
for the branch, its number and state are shown; clicking opens it in the
browser. GitHub integration uses the `gh` CLI and requires it to be installed
and authenticated.

## Build and run

```bash
cargo build
cargo run
```

## Tests

```bash
cargo test
cargo clippy --all-targets
```
