# AI agent mode in repterm — design

## Intent and why

The user wants to switch between the terminal and a conversation with an AI agent without leaving repterm, using `Ctrl+Shift+Enter` or a `!` prefix. The agent is for conversing about the real state of the repo (directory, branch, pending changes, and terminal output) and for proposing commands. The provider will be the LiteLLM proxy the user already has running in Docker, with a settings interface inside the app itself.

This specification covers the first version: **converse and propose**. It does not include tools or automatic execution.

## Goals

- Switch between terminal mode and chat mode with `Ctrl+Shift+Enter` at any time, preserving the PTY and history as-is.
- Switch by typing `!`: in the terminal it opens the chat; in the chat it returns to the terminal, and `!command` writes that command to the active PTY and presses Enter (executing it). The «Insert into terminal» button on code blocks, by contrast, inserts without executing.
- Converse with a model served by LiteLLM via the OpenAI-compatible API, with streaming responses.
- Configure the base URL, key, and model from the repterm interface, saving them locally.
- Send as context the active directory, the branch, the complete list of pending files from `RepoStatus.changes` (path, staged status, +/− lines), the terminal selection if any, and the last visible lines.
- Show proposed commands as code blocks with an action to insert them into the terminal (without executing them).

## Non-goals

- Tool calls, file reading/writing by the model, or automatic execution of commands.
- Support for providers other than LiteLLM in this version.
- Branch creation, stage/discard/commit, or any other git action (the current integration only lists and shows diffs).
- Images in markdown, heavy external syntax highlighting, or synchronization of the conversation with opencode/claude sessions.

## Constraints

- Rust 2021, current edition and toolchain (rustc 1.99.1 available).
- eframe/egui 0.31; `egui_term` vendored in `vendor/egui_term`. Avoid changes to vendor if possible.
- Git polling already uses `std::thread` + `mpsc`; the HTTP client should follow that pattern (without adding tokio).
- Focus is managed by `TerminalView::set_focus(...)` in `src/app.rs:739` and `egui_term` requests focus every frame when it has `has_focus`; any new TextEdit must be protected from that focus theft, as was done with the branch picker.
- Existing shortcuts in `src/app.rs:665-687` (`Ctrl+T/W/Delete/PageUp/PageDown`) must keep working and have explicit priority over the new ones.

## Decisions made

1. **Provider.** LiteLLM proxy, OpenAI-compatible API: `POST {base_url}/chat/completions` with `stream: true`, deltas in `choices[0].delta.content`, end at `data: [DONE]`. Model discovery via `GET {base_url}/models` (shape `{ "data": [ { "id": "..." } ] }`).
2. **HTTP client.** `ureq = { version = "3.5", features = ["json"] }` (blocking, rustls by default, MSRV 1.85 < 1.99). Read the body as `Read` from a thread, join buffers, and parse SSE frames separated by `\n\n`. An `AtomicBool` allows canceling the streaming from the UI.
3. **Settings.** File `$XDG_CONFIG_HOME/repterm/settings.json` (or `~/.config/repterm/settings.json`), serialized with `serde`/`serde_json` (already added for the API). Fields: `base_url` (default `http://localhost:4000/v1`), `api_key`, `model`, `include_terminal_context: bool`. `0600` permissions when creating the file on Unix. The key is not written to logs or shown in the conversation; the settings field is preloaded masked from disk and can be revealed with the eye button.
4. **`!` interaction.** It is intercepted only when the current terminal line is empty (no characters typed since the last Enter), so as not to break bash history expansion. Verified mechanism: `egui_term` clones `ctx` events in `vendor/egui_term/src/view.rs:152-154`, so `App::update` can remove `Event::Text("!")` from `ctx.input_mut().events` before drawing the panels, without touching vendor. When detected, it switches to chat and moves focus to the composer; the rest of what the user types goes to the chat. In chat, `!` at the start of the composer returns to the terminal; if there is text after it, it is written to the active PTY and Enter is pressed.
5. **Context and request.** It is built in the UI before launching the request: cwd of the active tab, `format_git_label` + list of `RepoStatus.changes`, `backend.selectable_content()` selection if any, and at most the last 120 visible lines of the grid. This context is sent as a `system` message **on every turn** (refreshed), followed by the history; at most the last 20 messages are resent to control cost and limits. `include_terminal_context = false` omits both the 120 lines and the selection; cwd, branch, and pending files are always sent. Hard character limit for the terminal block.
6. **Responses.** Simple markdown (text, lists, code) with `pulldown-cmark` 0.13 drawn via `LayoutJob`, to avoid coupling the egui version to `egui_commonmark`. Code blocks carry an «Insert into terminal» button that writes the text to the active PTY without Enter.
7. **Model state.** Selector in settings populated asynchronously from `/v1/models`, with manual entry as a fallback if the endpoint fails or requires different permissions.
8. **Conversation scope.** A single global in-memory conversation, shared by all tabs; it is not persisted on restart. Proposed commands and `!cmd` always go to the tab active at the time of use; if there is no active tab, a warning is shown and nothing is written.
9. **System prompt.** The implementer drafts a brief prompt that sets the role («assistant inside a terminal»), requires fenced code blocks for commands, forbids claiming to have tools, and asks for concise answers. It is not exposed as a setting.
10. **Cancellation and streaming.** The send button is disabled while a stream is in progress (messages are not queued). Canceling keeps the partial text as an assistant message marked «canceled» and re-enables the composer; the thread is stopped via the `AtomicBool`.

## Interface (designer brief)

- Three mutually exclusive surfaces: terminal (`CentralPanel`), chat (`CentralPanel`, replaces `TerminalView`), and settings (modal `Window` of ~460×340). The tab bar and git chips remain visible at all times.
- Settings: modal with Base URL, masked API key with reveal option, and model `ComboBox` populated from `/v1/models` with manual entry as fallback; Save/Cancel buttons; `Escape` closes without saving; states `loading_models`, `saving`, `saved`, and `save_error`. Opening settings closes the branch picker; the modal captures all keyboard input.
- Chat: ~34 px header with «AI chat», status dot (green connected / amber streaming / red error), read-only model, and × button; message list with user bubbles (violet `(116,93,215)`) and assistant bubbles (`weak_bg_fill`), simple markdown, blinking block cursor during streaming, and `!cmd` echoes as a monospace chip; 13 px monospace composer (44–160 px tall), `Enter` sends and `Shift+Enter` inserts a newline.
- Keyboard precedence: 1) settings modal; 2) `Ctrl+Shift+Enter`; 3) tab shortcuts; 4) `!` in terminal with a clean line; 5) PTY events; 6) `Enter` in the composer (`!cmd` returns to the terminal and executes); 7) send to the model.
- In chat mode, `TerminalView` is not added to the widget tree, so it cannot steal focus from the composer. Reinforcement: `.set_focus(self.picker.is_none() && !chat_mode)`.
- Spacing and palette: `item_spacing` 4 (tabs), 6 (git chips), and 8 (chat header); monospace 12 in messages; bubbles with `10×12` padding, `corner_radius(10)`, and a maximum width of 70% of the central area; ADD `(95,184,106)`, DEL `(224,108,108)`, WARN `(217,164,65)`, and CHAT_ACCENT `(116,93,215)`.

## Left to the implementer

- The exact widget layout, spacing, colors, loading/error states, and keyboard precedence are set by the designer in their own brief; this spec only defines behavior.
- Implementation in two independent lanes: API/SSE backend + settings (`src/llm.rs`, `src/settings.rs`) and chat mode condensation in `src/app.rs`. Do not overlap write scopes.
- «Clean line» detection is implemented with a per-tab flag (true when the tab is created and after each Enter, false when any other text is typed), not by inspecting the grid; `!` is removed from the event stream before `TerminalView` sees it.
- Unit tests: SSE parser (multiple deltas, `[DONE]`, error event), settings load/save, context construction with/without selection, mode toggle, and prefix detection.

## Known risks

- The key is stored in plain text in the settings file (`0600`); this is the trade-off of the chosen «settings in the app» option.
- Terminal output may contain secrets; that is why `include_terminal_context` exists.
- A `!` intercepted at the wrong moment can prevent legitimate bash commands with history; the «only with an empty line» rule limits the damage.
