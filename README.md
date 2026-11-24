# repterm

[![ci](https://github.com/arepresas/repterm/actions/workflows/ci.yml/badge.svg)](https://github.com/arepresas/repterm/actions/workflows/ci.yml)
[![release](https://github.com/arepresas/repterm/actions/workflows/release.yml/badge.svg)](https://github.com/arepresas/repterm/actions/workflows/release.yml)

A fast, simple terminal emulator built with Rust, egui, and egui_term.

## Features

- Multiple tabs with keyboard and mouse controls
- Git and GitHub status in a bar below the tabs
- Clickable branch chip with a searchable local/remote branch picker
- Clickable changes chip with a staged/unstaged file list and unified diffs
- AI chat mode backed by a LiteLLM proxy, with streaming answers and repository context
- Proposed commands are inserted into the terminal by hand — nothing is executed automatically
- OSC 52 clipboard support
- Configurable shell via the `SHELL` environment variable (Oh My Zsh recommended — see [Shell setup](#shell-setup-oh-my-zsh))

## Controls

| Action | Shortcut |
| --- | --- |
| New tab | `Ctrl+T` |
| Close tab | `Ctrl+W`, `Ctrl+Delete`, middle-click, or the `×` button |
| Previous tab | `Ctrl+PageUp` |
| Next tab | `Ctrl+PageDown` |
| Delete word | `Ctrl+Backspace` |
| Toggle AI chat / terminal | `Ctrl+Shift+Enter` |
| Ask AI without leaving the terminal | `!question`, then `Enter` |
| Run a proposed command from AI chat | `!command`, then `Enter` |
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

In the terminal, type `!question` and press Enter to ask the assistant without
leaving the terminal. The question and answer stay in the terminal scrollback,
like shell output. After that, keep typing replies without `!` until `Esc`
returns to the normal shell. Press `Ctrl+Shift+Enter` for the full chat.

Each request includes the active tab's path, branch and changed files. Terminal
output and the current selection are also included only when "Incluir contexto
del terminal" is enabled. Answers may contain fenced code blocks; in full chat,
use "Insertar en terminal" to copy one into the active terminal, or type
`!command` in the composer to write and run it there. The assistant has no tool
access and never executes commands by itself.

## Git and GitHub

The status bar reads the active tab's working directory and shows the current
branch, dirty state, and ahead/behind counts. If a GitHub pull request exists
for the branch, its number and state are shown; clicking opens it in the
browser. GitHub integration uses the `gh` CLI and requires it to be installed
and authenticated.

## Install on Ubuntu

Download the `.deb` for `amd64` from the [latest release](https://github.com/arepresas/repterm/releases/latest) and install it with:

```bash
sudo apt install ./repterm_*_amd64.deb
```

The package installs `/usr/bin/repterm`, depends on `zsh`, and always starts new tabs in zsh. If zsh is missing it exits with an explanation instead of starting. The `.tar.gz` archives are still published alongside it for other distributions and architectures.

## Build and run

```bash
cargo build
cargo run
```

## Shell setup: Oh My Zsh

Each tab runs your `$SHELL`, so using zsh with Oh My Zsh is the recommended way
to get completions, shared history and plugins inside repterm. Install it with:

```bash
sh -c "$(curl -fsSL https://raw.githubusercontent.com/ohmyzsh/ohmyzsh/master/tools/install.sh)"
```

Two useful additions are `zsh-autosuggestions` and `zsh-syntax-highlighting`:

```bash
git clone https://github.com/zsh-users/zsh-autosuggestions ${ZSH_CUSTOM:-~/.oh-my-zsh/custom}/plugins/zsh-autosuggestions
git clone https://github.com/zsh-users/zsh-syntax-highlighting ${ZSH_CUSTOM:-~/.oh-my-zsh/custom}/plugins/zsh-syntax-highlighting
```

Then enable them in `~/.zshrc`:

```zsh
plugins=(git zsh-autosuggestions zsh-syntax-highlighting)
ZSH_THEME="robbyrussell"
```

Start repterm with `SHELL=/bin/zsh cargo run` (or export `SHELL=/bin/zsh` in your
login shell) so new tabs pick it up. A few things to keep in mind:

- repterm already shows branch, dirty state and PR status in its status bar.
  Prefer a lightweight theme or disable the git segment in your prompt to avoid
  showing the same information twice.
- A `!` at the start of a clean line opens the AI chat; elsewhere it is left to
  zsh history expansion, so commands like `git show HEAD!` still behave normally.
- Oh My Zsh aliases and tab completion work as usual because they run inside the
  tab's PTY. Stick to ASCII-only prompt symbols unless you configure a font with
  the glyphs you need.

## Tests

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build
```

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

MIT. See [LICENSE](LICENSE).
