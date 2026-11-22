# repterm

A fast, simple terminal emulator built with Rust, egui, and egui_term.

## Features

- Multiple tabs with keyboard and mouse controls
- Git and GitHub status in a bar below the tabs
- Clickable branch chip with a searchable local/remote branch picker
- Clickable changes chip with a staged/unstaged file list and unified diffs
- AI chat mode backed by a LiteLLM proxy, with streaming answers and repository context
- Proposed commands are inserted into the terminal by hand — nothing is executed automatically
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
| Toggle AI chat / terminal | `Ctrl+Shift+Enter` |
| Open AI chat from the terminal | `!` at the start of a line |
| Run a proposed command | `!command` in the chat, then `Enter` |
| Send a chat message | `Enter` |
| Newline in the chat | `Shift+Enter` |
| Cancel a streaming answer | "Cancelar" button in the chat |

Click the branch chip to switch branches (writes `git checkout <branch>` into the active terminal), or the changes chip to open the diff panel. In the branch picker, type to filter, use `↑`/`↓` to navigate, `Enter` to select and `Esc` to cancel.

## AI agent mode

The chat talks to an OpenAI-compatible LiteLLM proxy. Use the "Ajustes" button in
the chat header to set the base URL (default `http://localhost:4000/v1`), API
key and model; models are discovered from `GET {base_url}/models`, with manual
entry as a fallback. Settings are stored in `$XDG_CONFIG_HOME/repterm/settings.json`
(or `~/.config/repterm/settings.json`) with `0600` permissions on Unix.

Each request includes the active tab's path, branch and changed files, plus the
last 120 terminal lines and the current selection unless "Incluir contexto del
terminal" is disabled. Answers are streamed and may contain fenced code blocks;
use "Insertar en terminal" to copy one into the active terminal, or type
`!command` in the composer to write and run it there. The assistant has no tool
access and never executes commands by itself.

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
